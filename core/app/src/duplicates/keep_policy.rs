//! Застосування KeepPolicy до підтверджених груп (T-065).
//!
//! Core обчислює ✓/╳; UI лише показує (architecture.md §4).

use std::collections::HashMap;

use trashradar_domain::candidate::{CandidateId, FileRecord};
use trashradar_domain::duplicates::{
    mark_content_hash_group, ContentHashGroup, DuplicateMemberRef, KeepPolicy, MarkedDuplicateGroup,
};

/// Побудувати MemberRef з індексних записів.
pub fn member_refs_from_records(
    ids: &[CandidateId],
    records: &HashMap<CandidateId, &FileRecord>,
) -> Vec<DuplicateMemberRef> {
    ids.iter()
        .map(|id| {
            if let Some(r) = records.get(id) {
                DuplicateMemberRef {
                    candidate_id: *id,
                    path: r.path.clone(),
                    modified_at: r.modified_at,
                }
            } else {
                DuplicateMemberRef {
                    candidate_id: *id,
                    path: format!("#{}", id.0),
                    modified_at: None,
                }
            }
        })
        .collect()
}

/// Індекс path/mtime за candidate_id.
pub fn record_index_by_id(records: &[FileRecord]) -> HashMap<CandidateId, &FileRecord> {
    records.iter().map(|r| (r.candidate_id, r)).collect()
}

/// Розмітити одну content-групу за політикою.
pub fn mark_group_with_records(
    group: &ContentHashGroup,
    records: &HashMap<CandidateId, &FileRecord>,
    policy: KeepPolicy,
) -> MarkedDuplicateGroup {
    let refs = member_refs_from_records(&group.members, records);
    mark_content_hash_group(group, &refs, policy)
}

/// Розмітити всі підтверджені групи (DoD: UI отримує готову ✓/╳).
pub fn mark_confirmed_groups(
    groups: &[ContentHashGroup],
    records: &[FileRecord],
    policy: KeepPolicy,
) -> Vec<MarkedDuplicateGroup> {
    let by_id = record_index_by_id(records);
    groups
        .iter()
        .map(|g| mark_group_with_records(g, &by_id, policy))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use trashradar_domain::candidate::{
        ByteSize, CandidateUnit, Decision, FileAttributes, FileKind, FsTimestamp, SafetyLevel,
    };
    use trashradar_domain::category::CategoryId;
    use trashradar_domain::duplicates::{ContentHash, DuplicateRole};

    fn rec(id: u64, path: &str, mtime: Option<i64>) -> FileRecord {
        FileRecord {
            candidate_id: CandidateId(id),
            path: path.into(),
            size: ByteSize(100),
            created_at: None,
            modified_at: mtime.map(FsTimestamp),
            accessed_at: None,
            kind: FileKind::Other,
            unit: CandidateUnit::File,
            category: CategoryId::Uncategorized,
            safety: SafetyLevel::ReviewRecommended,
            decision: Decision::Undecided,
            detector_id: String::new(),
            explanation: String::new(),
            attributes: FileAttributes::default(),
        }
    }

    fn group(ids: &[u64]) -> ContentHashGroup {
        ContentHashGroup {
            size: ByteSize(100),
            content_hash: ContentHash([1u8; 32]),
            members: ids.iter().map(|&i| CandidateId(i)).collect(),
        }
    }

    #[test]
    fn prefer_outside_downloads() {
        let records = [
            rec(1, r"C:\Users\Ada\Downloads\copy.mp4", Some(10)),
            rec(2, r"C:\Media\archive\copy.mp4", Some(20)), // newer but outside
        ];
        let g = group(&[1, 2]);
        let marked = mark_confirmed_groups(&[g], &records, KeepPolicy::PreferOutsideDownloads);
        assert_eq!(marked[0].keep_id, CandidateId(2));
        assert!(marked[0].members.iter().find(|m| m.keep).unwrap().keep);
        let keep = marked[0].members.iter().find(|m| m.keep).unwrap();
        let reap = marked[0].members.iter().find(|m| !m.keep).unwrap();
        assert_eq!(keep.role, DuplicateRole::Keep);
        assert_eq!(reap.role, DuplicateRole::Reap);
        assert_eq!(keep.role.mark_symbol(), '✓');
        assert_eq!(reap.role.mark_symbol(), '╳');
        assert!(marked[0].markup_line().contains('✓'));
        assert!(marked[0].markup_line().contains('╳'));
    }

    #[test]
    fn prefer_oldest_modified() {
        let records = [
            rec(1, r"C:\a\x.bin", Some(100)),
            rec(2, r"C:\b\x.bin", Some(50)), // older
            rec(3, r"C:\c\x.bin", Some(200)),
        ];
        let marked = mark_confirmed_groups(
            &[group(&[1, 2, 3])],
            &records,
            KeepPolicy::PreferOldestModified,
        );
        assert_eq!(marked[0].keep_id, CandidateId(2));
        assert_eq!(marked[0].members.iter().filter(|m| m.keep).count(), 1);
        assert_eq!(marked[0].members.iter().filter(|m| !m.keep).count(), 2);
    }

    #[test]
    fn prefer_shortest_path() {
        let records = [
            rec(1, r"C:\very\long\path\to\file.dat", Some(1)),
            rec(2, r"C:\file.dat", Some(1)),
        ];
        let marked =
            mark_confirmed_groups(&[group(&[1, 2])], &records, KeepPolicy::PreferShortestPath);
        assert_eq!(marked[0].keep_id, CandidateId(2));
    }

    #[test]
    fn always_exactly_one_keep() {
        let records = [rec(1, r"C:\a", Some(1)), rec(2, r"C:\b", Some(1))];
        for policy in [
            KeepPolicy::PreferOldestModified,
            KeepPolicy::PreferNewestModified,
            KeepPolicy::PreferOutsideDownloads,
            KeepPolicy::PreferShortestPath,
        ] {
            let m = mark_confirmed_groups(&[group(&[1, 2])], &records, policy);
            assert_eq!(
                m[0].members.iter().filter(|x| x.keep).count(),
                1,
                "{policy:?}"
            );
        }
    }
}
