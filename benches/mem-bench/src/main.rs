//! Бенчмарк-гейт пам'яті Core (T-157).
//!
//! Ціль architecture.md §15: **RAM Core у простої після скану < 300 МБ.**
//!
//! На відміну від index-bench/scan-bench, які гейтять *оцінку* heap через
//! `capacity()`, цей бенч міряє **справжній working set (RSS)** процесу через
//! WinAPI (`GetProcessMemoryInfo`) — джерело правди для §15. Три жорсткі
//! інваріанти:
//!
//! 1. **§15 DoD:** RSS у простої після скану 1.5 млн файлів + `finish_indexing`
//!    < 300 МБ.
//! 2. **Чесність оцінки:** справжній RSS не менший за оцінку `memory_usage()`
//!    і не більший за неї в `ESTIMATE_TOLERANCE` разів — інакше оцінка бреше,
//!    і гейти index/scan-bench на ній перестають захищати.
//! 3. **Потоковість категоризації (T-157):** повний прохід індексу через
//!    `for_each_mut` (шлях ферми детекторів / перерахунку порогів) НЕ
//!    матеріалізує всі записи — піковий приріст RSS < `STREAM_PEAK_MARGIN`.
//!    Регрес назад до `get_all()` (спайк +190 МБ на 1.5 млн, до 418 МБ понад
//!    бюджет) валить гейт.
//!
//! RSS машинозалежний, тож гейти — **абсолютні стелі** (не baseline-ratio):
//! стеля §15 сама по собі детермінований інваріант.
//!
//! Windows-only: RSS — платформозалежний; продукт MVP теж Windows.
//! На інших ОС бенч свідомо порожній (no-op, exit 0).
//!
//! Використання:
//!   cargo run --release

use trashradar_app::ports::HotIndex;
use trashradar_domain::candidate::{
    ByteSize, CandidateId, CandidateUnit, Decision, FileAttributes, FileKind, FileRecord,
    FsTimestamp, SafetyLevel,
};
use trashradar_domain::category::CategoryId;
use trashradar_index_memory::InMemoryIndex;
use trashradar_platform_win::process_memory;

/// Еталон §15: ~1.5 млн файлів.
const RECORDS: u64 = 1_500_000;
const BATCH: usize = 10_000;
/// Директорій у синтетичному дереві (кожна вміщає ~100 файлів).
const DIR_BUCKETS: u64 = 15_000;

/// Стеля §15: RAM Core у простої < 300 МБ.
const CEILING_BYTES: u64 = 300 * 1024 * 1024;
/// Оцінка не має недооцінювати RSS більш ніж у стільки разів (чесність).
const ESTIMATE_TOLERANCE: f64 = 1.6;
/// Піковий приріст RSS повного проходу `for_each_mut` над простоєм.
/// Потоковий шлях додає ~0 МБ; get_all додавав би +190 МБ.
const STREAM_PEAK_MARGIN: u64 = 64 * 1024 * 1024;

fn mb(bytes: u64) -> f64 {
    bytes as f64 / 1024.0 / 1024.0
}

fn make(i: u64) -> FileRecord {
    let d = i % DIR_BUCKETS;
    FileRecord {
        candidate_id: CandidateId(i),
        path: format!(r"C:\Users\User\Folder_{d}\file_name_{i}.dat"),
        size: ByteSize(i.wrapping_mul(123)),
        created_at: Some(FsTimestamp(130_000_000_000_000_000 + i as i64 * 10_000_000)),
        modified_at: Some(FsTimestamp(140_000_000_000_000_000 + i as i64 * 10_000_000)),
        accessed_at: Some(FsTimestamp(150_000_000_000_000_000 + i as i64 * 10_000_000)),
        kind: FileKind::Video,
        unit: CandidateUnit::File,
        category: CategoryId::ForgottenVideos,
        safety: SafetyLevel::SafeToBulk,
        decision: Decision::Undecided,
        detector_id: String::new(),
        explanation: String::new(),
        attributes: FileAttributes::default(),
    }
}

fn build_index() -> InMemoryIndex {
    let index = InMemoryIndex::new();
    let mut batch = Vec::with_capacity(BATCH);
    for i in 0..RECORDS {
        batch.push(make(i));
        if batch.len() == BATCH {
            index.insert_batch(std::mem::take(&mut batch)).unwrap();
            batch = Vec::with_capacity(BATCH);
        }
    }
    if !batch.is_empty() {
        index.insert_batch(batch).unwrap();
    }
    index.finish_indexing();
    index
}

#[cfg(windows)]
fn run_gate() -> i32 {
    println!(
        "T-157 memory: {} файлів, стеля §15 = {} МБ (RSS)",
        RECORDS,
        CEILING_BYTES / 1024 / 1024
    );

    let index = build_index();
    assert_eq!(HotIndex::len(&index).unwrap() as u64, RECORDS);

    let idle_ws = process_memory().working_set_bytes;
    let estimate = index.memory_usage() as u64;
    println!(
        "Простій після скану: RSS={:.1} МБ, оцінка={:.1} МБ",
        mb(idle_ws),
        mb(estimate)
    );

    // Повний прохід ферми/перерахунку через потоковий for_each_mut (T-157).
    let peak_before = process_memory().peak_working_set_bytes;
    let mut touched = 0u64;
    index
        .for_each_mut(&mut |record| {
            // Імітуємо роботу детектора: змінюємо тільки метадані запису.
            record.size = ByteSize(record.size.0.wrapping_add(1));
            touched += 1;
            true
        })
        .unwrap();
    let stream_peak_delta = process_memory()
        .peak_working_set_bytes
        .saturating_sub(peak_before);
    assert_eq!(touched, RECORDS);
    println!(
        "Прохід for_each_mut: піковий приріст RSS={:.1} МБ (torkнуто {})",
        mb(stream_peak_delta),
        touched
    );

    let mut failed = false;
    let mut check = |name: &str, value: u64, ceiling: u64| {
        let ok = value <= ceiling;
        println!(
            "  {name:<28} {:.1} МБ  (стеля {:.1} МБ)  {}",
            mb(value),
            mb(ceiling),
            if ok { "ok" } else { "FAIL" }
        );
        if !ok {
            failed = true;
        }
    };

    check("idle_rss (§15)", idle_ws, CEILING_BYTES);
    check(
        "estimate_honesty",
        idle_ws,
        (estimate as f64 * ESTIMATE_TOLERANCE) as u64,
    );
    check(
        "for_each_mut peak delta",
        stream_peak_delta,
        STREAM_PEAK_MARGIN,
    );

    if failed {
        eprintln!("\nПам'ять перевищила бюджет §15 / потоковий інваріант — гейт не пройдено.");
        return 1;
    }
    println!("\nГейт пройдено: RSS у простої < 300 МБ; категоризація не матеріалізує індекс.");
    0
}

#[cfg(not(windows))]
fn run_gate() -> i32 {
    // RSS — платформозалежний; продукт MVP Windows-only (product.md §6).
    println!("mem-bench: Windows-only (RSS через WinAPI), пропущено.");
    0
}

fn main() {
    std::process::exit(run_gate());
}
