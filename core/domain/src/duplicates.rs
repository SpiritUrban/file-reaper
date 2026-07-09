//! Каскад пошуку дублікатів (architecture.md §4).
//!
//! ```text
//! УСІ ФАЙЛИ (індекс, 0 I/O)
//!   │  щабель 1: групування за точним розміром          (T-058)
//!   ▼     унікальний розмір → НЕ дублікат (~95%)
//! ГРУПИ ОДНАКОВОГО РОЗМІРУ
//!   │  щабель 2: частковий хеш (перші+останні 64 КБ)  (T-059)
//!   ▼     різний partial → НЕ дублікат
//! ГРУПИ-КАНДИДАТИ
//!   │  щабель 3: повний потоковий BLAKE3                 (T-060)
//!   ▼     різний content hash → НЕ дублікат
//! ПІДТВЕРДЖЕНІ ГРУПИ ДУБЛІКАТІВ
//! ```
//!
//! DoD T-058: унікальний розмір відкинутий; 1 млн < 1 с.
//! DoD T-059: групи з різними partial-хешами розділяються; ≤ 128 КБ/файл (I/O у infra).
//! DoD T-060: повний хеш — константна пам'ять на файл; I/O-bound (не CPU).

use serde::{Deserialize, Serialize};

use crate::candidate::{ByteSize, CandidateId};

/// Розмір одного «кінця» файла для щабля 2 (architecture.md §4).
pub const PARTIAL_HASH_CHUNK_BYTES: u64 = 64 * 1024;

/// Максимум байтів, які дозволено прочитати з диска на файл у щаблі 2.
pub const PARTIAL_HASH_MAX_READ_BYTES: u64 = PARTIAL_HASH_CHUNK_BYTES * 2;

/// Вхід щабля 1: ідентичність + розмір (без шляху — економія).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SizeKey {
    pub candidate_id: CandidateId,
    pub size: ByteSize,
}

/// Група файлів **однакового** розміру (≥ 2 члени) — кандидати на щабель 2.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExactSizeGroup {
    pub size: ByteSize,
    pub members: Vec<CandidateId>,
}

impl ExactSizeGroup {
    /// Скільки можна звільнити, залишивши 1 екземпляр: `size × (n − 1)`.
    pub fn potential_reclaim_bytes(&self) -> u64 {
        let n = self.members.len() as u64;
        self.size.0.saturating_mul(n.saturating_sub(1))
    }

    pub fn member_count(&self) -> usize {
        self.members.len()
    }
}

/// Згрупувати за точним розміром; відкинути унікальні розміри та size==0.
///
/// - **0 I/O** — лише метадані;
/// - порядок груп: більший потенціал звільнення спочатку (підготовка T-064);
/// - усередині групи — стабільний порядок `candidate_id`.
pub fn group_by_exact_size(files: impl IntoIterator<Item = SizeKey>) -> Vec<ExactSizeGroup> {
    use std::collections::HashMap;

    // size → member ids (один прохід, 0 I/O)
    let mut buckets: HashMap<u64, Vec<CandidateId>> = HashMap::new();
    for f in files {
        if f.size.0 == 0 {
            continue;
        }
        buckets.entry(f.size.0).or_default().push(f.candidate_id);
    }

    let mut groups: Vec<ExactSizeGroup> = buckets
        .into_iter()
        .filter_map(|(size, mut members)| {
            if members.len() < 2 {
                return None;
            }
            members.sort_unstable_by_key(|id| id.0);
            // дедуп id (на випадок дубль-рядків індексу)
            members.dedup();
            if members.len() < 2 {
                return None;
            }
            Some(ExactSizeGroup {
                size: ByteSize(size),
                members,
            })
        })
        .collect();

    groups.sort_unstable_by(|a, b| {
        b.potential_reclaim_bytes()
            .cmp(&a.potential_reclaim_bytes())
            .then_with(|| b.size.0.cmp(&a.size.0))
            .then_with(|| a.members[0].0.cmp(&b.members[0].0))
    });
    groups
}

/// Підсумок щабля 1 (для UI «попередня цифра» / метрик).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SizeStageStats {
    pub files_seen: u64,
    pub files_unique_size: u64,
    pub files_in_groups: u64,
    pub group_count: u64,
    /// Σ size×(n−1) по групах.
    pub potential_reclaim_bytes: u64,
}

impl SizeStageStats {
    pub fn from_groups(files_seen: u64, groups: &[ExactSizeGroup]) -> Self {
        let files_in_groups: u64 = groups.iter().map(|g| g.member_count() as u64).sum();
        let potential_reclaim_bytes = groups.iter().map(|g| g.potential_reclaim_bytes()).sum();
        Self {
            files_seen,
            files_unique_size: files_seen.saturating_sub(files_in_groups),
            files_in_groups,
            group_count: groups.len() as u64,
            potential_reclaim_bytes,
        }
    }
}

// ─── Щабель 2: частковий хеш (T-059) ─────────────────────────────────────────

/// 32-байтний відбиток (BLAKE3 від head‖tail) — без I/O у domain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PartialHash(pub [u8; 32]);

impl PartialHash {
    pub const ZERO: Self = Self([0u8; 32]);

    /// Стабільний hex (64 символи) для логів / IPC.
    pub fn to_hex(self) -> String {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut out = String::with_capacity(64);
        for b in self.0 {
            out.push(HEX[(b >> 4) as usize] as char);
            out.push(HEX[(b & 0xf) as usize] as char);
        }
        out
    }
}

/// Вхід щабля 2: id + розмір + partial fingerprint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartialHashKey {
    pub candidate_id: CandidateId,
    pub size: ByteSize,
    pub partial_hash: PartialHash,
}

/// Група однакового розміру **і** partial-хешу (≥ 2) — кандидати на щабель 3.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartialHashGroup {
    pub size: ByteSize,
    pub partial_hash: PartialHash,
    pub members: Vec<CandidateId>,
}

impl PartialHashGroup {
    pub fn potential_reclaim_bytes(&self) -> u64 {
        let n = self.members.len() as u64;
        self.size.0.saturating_mul(n.saturating_sub(1))
    }

    pub fn member_count(&self) -> usize {
        self.members.len()
    }
}

/// Згрупувати за (size, partial_hash); синглтони відкинути.
///
/// DoD T-059: файли з **різними** partial-хешами (навіть при однаковому size)
/// потрапляють у **різні** групи або відсіюються, якщо лишились самі.
pub fn group_by_partial_hash(
    files: impl IntoIterator<Item = PartialHashKey>,
) -> Vec<PartialHashGroup> {
    use std::collections::HashMap;

    // (size, hash) → members
    let mut buckets: HashMap<(u64, [u8; 32]), Vec<CandidateId>> = HashMap::new();
    for f in files {
        if f.size.0 == 0 {
            continue;
        }
        buckets
            .entry((f.size.0, f.partial_hash.0))
            .or_default()
            .push(f.candidate_id);
    }

    let mut groups: Vec<PartialHashGroup> = buckets
        .into_iter()
        .filter_map(|((size, hash), mut members)| {
            if members.len() < 2 {
                return None;
            }
            members.sort_unstable_by_key(|id| id.0);
            members.dedup();
            if members.len() < 2 {
                return None;
            }
            Some(PartialHashGroup {
                size: ByteSize(size),
                partial_hash: PartialHash(hash),
                members,
            })
        })
        .collect();

    groups.sort_unstable_by(|a, b| {
        b.potential_reclaim_bytes()
            .cmp(&a.potential_reclaim_bytes())
            .then_with(|| b.size.0.cmp(&a.size.0))
            .then_with(|| a.partial_hash.0.cmp(&b.partial_hash.0))
            .then_with(|| a.members[0].0.cmp(&b.members[0].0))
    });
    groups
}

/// Підсумок щабля 2.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PartialHashStageStats {
    /// Файли, для яких обчислено partial (або спроба).
    pub files_hashed: u64,
    /// Не вдалося прочитати / хешувати (пропуск).
    pub files_failed: u64,
    /// Після групування: синглтони (унікальний partial у size-групі).
    pub files_unique_partial: u64,
    pub files_in_groups: u64,
    pub group_count: u64,
    pub potential_reclaim_bytes: u64,
    /// Сума фактично прочитаних байтів (≤ 128 КіБ × files_hashed).
    pub bytes_read: u64,
    pub cancelled: bool,
}

impl PartialHashStageStats {
    pub fn from_groups(
        files_hashed: u64,
        files_failed: u64,
        bytes_read: u64,
        cancelled: bool,
        groups: &[PartialHashGroup],
    ) -> Self {
        let files_in_groups: u64 = groups.iter().map(|g| g.member_count() as u64).sum();
        let potential_reclaim_bytes = groups.iter().map(|g| g.potential_reclaim_bytes()).sum();
        Self {
            files_hashed,
            files_failed,
            files_unique_partial: files_hashed.saturating_sub(files_in_groups),
            files_in_groups,
            group_count: groups.len() as u64,
            potential_reclaim_bytes,
            bytes_read,
            cancelled,
        }
    }
}

// ─── Щабель 3: повний content hash (T-060) ───────────────────────────────────

/// Повний BLAKE3 файла (32 байти) — підтверджений дублікат.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ContentHash(pub [u8; 32]);

impl ContentHash {
    pub const ZERO: Self = Self([0u8; 32]);

    pub fn to_hex(self) -> String {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut out = String::with_capacity(64);
        for b in self.0 {
            out.push(HEX[(b >> 4) as usize] as char);
            out.push(HEX[(b & 0xf) as usize] as char);
        }
        out
    }
}

/// Вхід щабля 3 після повного хешу.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentHashKey {
    pub candidate_id: CandidateId,
    pub size: ByteSize,
    pub content_hash: ContentHash,
}

/// Підтверджена група дублікатів (однаковий size + content hash).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentHashGroup {
    pub size: ByteSize,
    pub content_hash: ContentHash,
    pub members: Vec<CandidateId>,
}

impl ContentHashGroup {
    pub fn potential_reclaim_bytes(&self) -> u64 {
        let n = self.members.len() as u64;
        self.size.0.saturating_mul(n.saturating_sub(1))
    }

    pub fn member_count(&self) -> usize {
        self.members.len()
    }
}

/// Згрупувати за (size, content_hash); синглтони відкинути.
pub fn group_by_content_hash(
    files: impl IntoIterator<Item = ContentHashKey>,
) -> Vec<ContentHashGroup> {
    use std::collections::HashMap;

    let mut buckets: HashMap<(u64, [u8; 32]), Vec<CandidateId>> = HashMap::new();
    for f in files {
        if f.size.0 == 0 {
            continue;
        }
        buckets
            .entry((f.size.0, f.content_hash.0))
            .or_default()
            .push(f.candidate_id);
    }

    let mut groups: Vec<ContentHashGroup> = buckets
        .into_iter()
        .filter_map(|((size, hash), mut members)| {
            if members.len() < 2 {
                return None;
            }
            members.sort_unstable_by_key(|id| id.0);
            members.dedup();
            if members.len() < 2 {
                return None;
            }
            Some(ContentHashGroup {
                size: ByteSize(size),
                content_hash: ContentHash(hash),
                members,
            })
        })
        .collect();

    groups.sort_unstable_by(|a, b| {
        b.potential_reclaim_bytes()
            .cmp(&a.potential_reclaim_bytes())
            .then_with(|| b.size.0.cmp(&a.size.0))
            .then_with(|| a.content_hash.0.cmp(&b.content_hash.0))
            .then_with(|| a.members[0].0.cmp(&b.members[0].0))
    });
    groups
}

/// Підсумок щабля 3.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContentHashStageStats {
    pub files_hashed: u64,
    pub files_failed: u64,
    pub files_unique_content: u64,
    pub files_in_groups: u64,
    pub group_count: u64,
    pub potential_reclaim_bytes: u64,
    /// Байти, прочитані під час повного хешу (≈ сума size успішних).
    pub bytes_read: u64,
    pub cancelled: bool,
    /// Скільки потоків файлів використано (1 = послідовно).
    pub file_workers: u32,
}

impl ContentHashStageStats {
    pub fn from_groups(
        files_hashed: u64,
        files_failed: u64,
        bytes_read: u64,
        cancelled: bool,
        file_workers: u32,
        groups: &[ContentHashGroup],
    ) -> Self {
        let files_in_groups: u64 = groups.iter().map(|g| g.member_count() as u64).sum();
        let potential_reclaim_bytes = groups.iter().map(|g| g.potential_reclaim_bytes()).sum();
        Self {
            files_hashed,
            files_failed,
            files_unique_content: files_hashed.saturating_sub(files_in_groups),
            files_in_groups,
            group_count: groups.len() as u64,
            potential_reclaim_bytes,
            bytes_read,
            cancelled,
            file_workers,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(id: u64, size: u64) -> SizeKey {
        SizeKey {
            candidate_id: CandidateId(id),
            size: ByteSize(size),
        }
    }

    #[test]
    fn unique_sizes_discarded() {
        // DoD: унікальний розмір → не в групах.
        let groups = group_by_exact_size([
            key(1, 100),
            key(2, 200),
            key(3, 300),
            key(4, 100), // пара з 1
        ]);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].size.0, 100);
        assert_eq!(groups[0].members.len(), 2);
        assert_eq!(groups[0].members[0].0, 1);
        assert_eq!(groups[0].members[1].0, 4);
    }

    #[test]
    fn zero_size_ignored() {
        let groups = group_by_exact_size([key(1, 0), key(2, 0), key(3, 10), key(4, 10)]);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].size.0, 10);
    }

    #[test]
    fn potential_reclaim_leaves_one_copy() {
        let g = ExactSizeGroup {
            size: ByteSize(50),
            members: vec![CandidateId(1), CandidateId(2), CandidateId(3)],
        };
        assert_eq!(g.potential_reclaim_bytes(), 100); // 50 * 2
    }

    #[test]
    fn larger_reclaim_sorted_first() {
        let groups = group_by_exact_size([
            key(1, 10),
            key(2, 10), // reclaim 10
            key(3, 1000),
            key(4, 1000),
            key(5, 1000), // reclaim 2000
        ]);
        assert_eq!(groups[0].size.0, 1000);
        assert_eq!(groups[0].potential_reclaim_bytes(), 2000);
        assert_eq!(groups[1].size.0, 10);
    }

    #[test]
    fn stats_count_unique_vs_grouped() {
        let keys = [
            key(1, 1),
            key(2, 2),
            key(3, 2),
            key(4, 3),
            key(5, 3),
            key(6, 3),
        ];
        let groups = group_by_exact_size(keys);
        let stats = SizeStageStats::from_groups(6, &groups);
        assert_eq!(stats.group_count, 2);
        assert_eq!(stats.files_in_groups, 5); // 2+3
        assert_eq!(stats.files_unique_size, 1); // id=1
        assert_eq!(stats.potential_reclaim_bytes, 2 + 3 * 2); // size2:1 + size3:2
    }

    /// DoD T-058: 1 млн записів < 1 с (release; debug ~2× повільніший).
    ///
    /// ```text
    /// cargo test -p trashradar-domain group_one_million --release -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "perf gate: cargo test -p trashradar-domain group_one_million --release -- --ignored --nocapture"]
    fn group_one_million_records_under_one_second() {
        const N: u64 = 1_000_000;
        let keys: Vec<SizeKey> = (0..N)
            .map(|i| {
                let size = if i < 100_000 {
                    (i / 2) + 1 // 50_000 груп по 2
                } else {
                    1_000_000 + i // унікальні
                };
                key(i, size)
            })
            .collect();

        let start = std::time::Instant::now();
        let groups = group_by_exact_size(keys.iter().copied());
        let elapsed = start.elapsed();

        assert_eq!(groups.len(), 50_000);
        let in_groups: usize = groups.iter().map(|g| g.member_count()).sum();
        assert_eq!(in_groups, 100_000);
        assert!(
            elapsed.as_secs_f64() < 1.0,
            "1M exact-size group took {elapsed:?} (DoD < 1s)"
        );
        eprintln!("T-058 group_by_exact_size 1M: {elapsed:?}");
    }

    fn phash(byte: u8) -> PartialHash {
        let mut a = [0u8; 32];
        a[0] = byte;
        PartialHash(a)
    }

    fn pkey(id: u64, size: u64, h: u8) -> PartialHashKey {
        PartialHashKey {
            candidate_id: CandidateId(id),
            size: ByteSize(size),
            partial_hash: phash(h),
        }
    }

    #[test]
    fn different_partial_hashes_split_same_size_group() {
        // DoD T-059: однаковий size, різний partial → різні групи / відсів.
        let groups = group_by_partial_hash([
            pkey(1, 1000, 0xAA),
            pkey(2, 1000, 0xAA), // пара AA
            pkey(3, 1000, 0xBB), // інший partial — синглтон
            pkey(4, 1000, 0xBB),
            pkey(5, 1000, 0xBB), // трійка BB
        ]);
        assert_eq!(groups.len(), 2);
        let aa = groups
            .iter()
            .find(|g| g.partial_hash == phash(0xAA))
            .unwrap();
        let bb = groups
            .iter()
            .find(|g| g.partial_hash == phash(0xBB))
            .unwrap();
        assert_eq!(aa.members.len(), 2);
        assert_eq!(bb.members.len(), 3);
        assert_eq!(bb.potential_reclaim_bytes(), 2000); // 1000 * 2
    }

    #[test]
    fn unique_partial_discarded() {
        let groups = group_by_partial_hash([
            pkey(1, 50, 1),
            pkey(2, 50, 1),
            pkey(3, 50, 9), // alone
        ]);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].members, vec![CandidateId(1), CandidateId(2)]);
    }

    #[test]
    fn partial_chunk_constants_match_architecture() {
        assert_eq!(PARTIAL_HASH_CHUNK_BYTES, 64 * 1024);
        assert_eq!(PARTIAL_HASH_MAX_READ_BYTES, 128 * 1024);
    }

    fn chash(byte: u8) -> ContentHash {
        let mut a = [0u8; 32];
        a[0] = byte;
        ContentHash(a)
    }

    fn ckey(id: u64, size: u64, h: u8) -> ContentHashKey {
        ContentHashKey {
            candidate_id: CandidateId(id),
            size: ByteSize(size),
            content_hash: chash(h),
        }
    }

    #[test]
    fn different_content_hashes_split_group() {
        let groups = group_by_content_hash([
            ckey(1, 500, 0x11),
            ckey(2, 500, 0x11),
            ckey(3, 500, 0x22),
            ckey(4, 500, 0x22),
            ckey(5, 500, 0x33), // alone
        ]);
        assert_eq!(groups.len(), 2);
        assert!(groups.iter().all(|g| g.members.len() >= 2));
        assert!(!groups.iter().any(|g| g.content_hash == chash(0x33)));
    }

    #[test]
    fn content_hash_hex_stable() {
        assert_eq!(chash(0xAB).to_hex().len(), 64);
        assert!(chash(0xAB).to_hex().starts_with("ab"));
    }
}
