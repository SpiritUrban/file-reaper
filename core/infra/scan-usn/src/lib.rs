//! Адаптер `ChangeSource`: USN Change Journal NTFS (T-029).
//!
//! - [`record`] — чистий парсер байтів `USN_RECORD_V2` (CI без прав);
//! - [`journal`] — QUERY/READ через WinAPI (потребує доступу до тому);
//! - позиція журналу зберігається в SQLite (`volume_usn_state`, index v2).
//!
//! Застосування дельти до індексу — `trashradar_app::usn_apply` (T-030).
//! Фолбек при `JournalStale` — T-031.

pub mod record;

#[cfg(windows)]
pub mod journal;

use trashradar_app::ports::{ChangeSource, UsnReadOutcome};
use trashradar_domain::error::CoreError;
use trashradar_domain::scan::{UsnCursor, UsnJournalInfo};

/// Джерело інкрементальних змін через USN Journal.
#[derive(Debug, Default, Clone, Copy)]
pub struct UsnChangeSource;

impl ChangeSource for UsnChangeSource {
    fn query_journal(&self, volume: char) -> Result<UsnJournalInfo, CoreError> {
        #[cfg(windows)]
        {
            journal::query_journal(volume)
        }
        #[cfg(not(windows))]
        {
            let _ = volume;
            Err(CoreError::not_implemented("usn.query_journal"))
        }
    }

    fn read_delta(&self, volume: char, from: UsnCursor) -> Result<UsnReadOutcome, CoreError> {
        #[cfg(windows)]
        {
            journal::read_delta(volume, from)
        }
        #[cfg(not(windows))]
        {
            let _ = (volume, from);
            Err(CoreError::not_implemented("usn.read_delta"))
        }
    }
}

/// Зафіксувати «кінець журналу» після повного скану (T-029 DoD).
///
/// Типовий виклик оркестратора: `capture_cursor_after_full_scan` →
/// `IndexStore::set_usn_cursor`.
pub fn capture_cursor_after_full_scan(
    source: &impl ChangeSource,
    volume: char,
) -> Result<UsnCursor, CoreError> {
    let info = source.query_journal(volume)?;
    Ok(info.cursor_at_end())
}

/// Прочитати дельту з збереженого курсора; при `JournalStale` — не оновлювати
/// курсор (оркестратор зробить повний рескан, T-031).
pub fn read_delta_from_store(
    source: &impl ChangeSource,
    volume: char,
    saved: UsnCursor,
) -> Result<UsnReadOutcome, CoreError> {
    source.read_delta(volume, saved)
}

#[cfg(test)]
mod tests {
    use super::*;
    use trashradar_domain::scan::UsnJournalInfo;

    #[test]
    fn cursor_at_end_matches_journal_next() {
        let info = UsnJournalInfo {
            journal_id: 7,
            lowest_valid_usn: 10,
            next_usn: 500,
            first_usn: 10,
        };
        let c = info.cursor_at_end();
        assert_eq!(c.journal_id, 7);
        assert_eq!(c.next_usn, 500);
        assert!(info.is_cursor_valid(c));
        assert!(!info.is_cursor_valid(UsnCursor {
            journal_id: 8,
            next_usn: 500
        }));
        assert!(!info.is_cursor_valid(UsnCursor {
            journal_id: 7,
            next_usn: 5
        }));
    }

    /// DoD T-029: після фіксації курсора дельта містить лише зміни;
    /// create/delete на томі відбивається в read_delta.
    #[test]
    #[ignore = "DoD T-029: потребує адмін-прав і NTFS; TR_MFT_TEST_DRIVE"]
    #[cfg(windows)]
    fn usn_delta_returns_only_new_changes() {
        use std::fs::{self, File};
        use std::io::Write;
        use std::path::PathBuf;
        use std::time::{SystemTime, UNIX_EPOCH};
        use trashradar_index_sqlite::IndexDatabase;

        let drive = std::env::var("TR_MFT_TEST_DRIVE")
            .ok()
            .and_then(|s| s.chars().next())
            .unwrap_or('F');

        let source = UsnChangeSource;
        let info = source
            .query_journal(drive)
            .expect("QUERY USN (потрібні права / NTFS)");

        // Зберігаємо курсор «зараз» у справжній SQLite — як після повного скану.
        let profile = std::env::temp_dir().join(format!(
            "tr-usn-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&profile).unwrap();
        let db = IndexDatabase::open_profile(&profile).expect("open index");
        let cursor = info.cursor_at_end();
        db.set_usn_cursor(drive, cursor).expect("save cursor");
        assert_eq!(db.get_usn_cursor(drive).unwrap(), Some(cursor));

        // Створюємо унікальний файл на томі → має з'явитись у дельті.
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let name = format!("trashradar_usn_probe_{stamp}.tmp");
        let path = PathBuf::from(format!("{}:\\{}", drive.to_ascii_uppercase(), name));
        {
            let mut f = File::create(&path).expect("create probe file on volume");
            f.write_all(b"usn-probe").unwrap();
        }

        // Невелика пауза: USN інколи з'являється після close.
        std::thread::sleep(std::time::Duration::from_millis(200));

        let saved = db.get_usn_cursor(drive).unwrap().expect("saved cursor");
        let outcome = source.read_delta(drive, saved).expect("read delta");

        let _ = fs::remove_file(&path);

        match outcome {
            UsnReadOutcome::JournalStale { reason, .. } => {
                panic!("несподіваний JournalStale: {reason}");
            }
            UsnReadOutcome::Changes {
                changes,
                next_cursor,
            } => {
                println!(
                    "Том {drive}: дельта {} записів; next_usn {} → {}",
                    changes.len(),
                    saved.next_usn,
                    next_cursor.next_usn
                );
                let found = changes.iter().any(|c| c.name == name);
                assert!(
                    found,
                    "дельта не містить створений файл {name}; sample: {:?}",
                    changes.iter().map(|c| &c.name).take(10).collect::<Vec<_>>()
                );
                // Курсор просунувся (або журнал уже мав next після create).
                assert!(next_cursor.next_usn >= saved.next_usn);
                assert_eq!(next_cursor.journal_id, saved.journal_id);
                db.set_usn_cursor(drive, next_cursor).expect("advance");
            }
        }

        let _ = fs::remove_dir_all(&profile);
    }
}
