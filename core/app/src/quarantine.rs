//! Application use cases Quarantine: транзакційний reap (T-079) і
//! restore на оригінальний шлях із суфіксом при конфлікті (T-080) —
//! architecture.md §7.3.

use std::path::Path;

use trashradar_domain::{
    error::CoreError,
    quarantine::{
        restore_destination, BatchId, FileIdentity, QuarantineEntry, QuarantineEntryId,
        QuarantineStatus, RESTORE_SUFFIX_MAX_ATTEMPTS,
    },
};

use crate::ports::{QuarantineFs, QuarantineManifest, RestoreMove};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReapRequest {
    pub entry: QuarantineEntry,
    pub destination_path: String,
    pub expected_identity: FileIdentity,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReapOutcome {
    pub entry: QuarantineEntry,
    pub destination_path: String,
}

pub struct TransactionalReaper<'a, F: QuarantineFs, M: QuarantineManifest> {
    filesystem: &'a F,
    manifest: &'a M,
    trashradar_roots: &'a [String],
}

impl<'a, F: QuarantineFs, M: QuarantineManifest> TransactionalReaper<'a, F, M> {
    pub fn new(filesystem: &'a F, manifest: &'a M, trashradar_roots: &'a [String]) -> Self {
        Self {
            filesystem,
            manifest,
            trashradar_roots,
        }
    }

    /// Двофазний reap: durable in_flight → atomic move → durable quarantined.
    pub fn reap_one(&self, mut request: ReapRequest) -> Result<ReapOutcome, CoreError> {
        validate_request(&request)?;
        request.entry.status = QuarantineStatus::InFlight;
        self.manifest.insert_entry(&request.entry)?;
        self.filesystem.move_into_quarantine(
            &request.entry.original_path,
            &request.destination_path,
            request.expected_identity,
            self.trashradar_roots,
        )?;
        self.manifest
            .update_status(request.entry.id, QuarantineStatus::Quarantined)?;
        request.entry.status = QuarantineStatus::Quarantined;
        Ok(ReapOutcome {
            entry: request.entry,
            destination_path: request.destination_path,
        })
    }

    /// Послідовний батч: перша помилка/аварія зупиняє нові move; вже завершені
    /// та поточний in_flight лишаються відновлюваними за manifest (T-084).
    pub fn reap_batch(
        &self,
        batch_id: BatchId,
        requests: Vec<ReapRequest>,
    ) -> Result<Vec<ReapOutcome>, CoreError> {
        requests
            .into_iter()
            .map(|mut request| {
                request.entry.batch_id = Some(batch_id);
                self.reap_one(request)
            })
            .collect()
    }
}

/// Запит на відновлення одного запису журналу (T-080).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreRequest {
    pub entry_id: QuarantineEntryId,
    /// Повний шлях суррогата у `<том>\.trashradar\quarantine` (резолвиться
    /// викликачем з `QuarantineDirectory` + `surrogate_name` manifest).
    pub surrogate_path: String,
}

/// Підсумок відновлення (T-080).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreOutcome {
    pub entry: QuarantineEntry,
    /// Фактичний шлях файла після відновлення.
    pub restored_path: String,
    /// Оригінальний шлях був зайнятий → застосовано суфікс; UI показує
    /// подію-попередження (DoD T-080).
    pub used_suffix: bool,
}

/// Use case відновлення: зворотний move + перехід manifest у `Restored`.
pub struct QuarantineRestorer<'a, F: QuarantineFs, M: QuarantineManifest> {
    filesystem: &'a F,
    manifest: &'a M,
}

impl<'a, F: QuarantineFs, M: QuarantineManifest> QuarantineRestorer<'a, F, M> {
    pub fn new(filesystem: &'a F, manifest: &'a M) -> Self {
        Self {
            filesystem,
            manifest,
        }
    }

    /// Відновлює файл на оригінальний шлях; зайнятий шлях → суфікс за
    /// доменним правилом [`restore_destination`]. Ціль ніколи не
    /// перезаписується (no-replace move у шлюзі).
    pub fn restore_one(&self, request: RestoreRequest) -> Result<RestoreOutcome, CoreError> {
        let mut entry = self.manifest.get_entry(request.entry_id)?.ok_or_else(|| {
            CoreError::invalid_argument(format!(
                "Запис карантину {} не знайдено.",
                request.entry_id.0
            ))
        })?;
        if entry.status != QuarantineStatus::Quarantined {
            return Err(CoreError::invalid_argument(format!(
                "Відновити можна лише запис у статусі quarantined (зараз {:?}).",
                entry.status
            )));
        }

        let mut attempt = 0u32;
        let (restored_path, used_suffix) = loop {
            let candidate = restore_destination(&entry.original_path, attempt);
            match self
                .filesystem
                .restore_from_quarantine(&request.surrogate_path, &candidate)?
            {
                RestoreMove::Restored => break (candidate, attempt > 0),
                RestoreMove::DestinationOccupied => {
                    attempt += 1;
                    if attempt >= RESTORE_SUFFIX_MAX_ATTEMPTS {
                        return Err(CoreError::io(format!(
                            "Не вдалося підібрати вільне ім'я для відновлення «{}» ({} спроб).",
                            entry.original_path, RESTORE_SUFFIX_MAX_ATTEMPTS
                        )));
                    }
                }
            }
        };

        self.manifest
            .update_status(entry.id, QuarantineStatus::Restored)?;
        entry.status = QuarantineStatus::Restored;
        Ok(RestoreOutcome {
            entry,
            restored_path,
            used_suffix,
        })
    }

    /// Послідовний батч відновлень (основа масового undo T-081).
    pub fn restore_batch(
        &self,
        requests: Vec<RestoreRequest>,
    ) -> Result<Vec<RestoreOutcome>, CoreError> {
        requests
            .into_iter()
            .map(|request| self.restore_one(request))
            .collect()
    }

    /// Масове «Скасувати»: manifest сам знаходить усі quarantined-записи
    /// операції, а викликач лише резолвить фізичний шлях сурогата.
    pub fn undo_batch(
        &self,
        batch_id: BatchId,
        mut surrogate_path: impl FnMut(&QuarantineEntry) -> Result<String, CoreError>,
    ) -> Result<Vec<RestoreOutcome>, CoreError> {
        let requests = self
            .manifest
            .list_entries_by_batch(batch_id)?
            .into_iter()
            .filter(|entry| entry.status == QuarantineStatus::Quarantined)
            .map(|entry| {
                Ok(RestoreRequest {
                    entry_id: entry.id,
                    surrogate_path: surrogate_path(&entry)?,
                })
            })
            .collect::<Result<Vec<_>, CoreError>>()?;
        self.restore_batch(requests)
    }
}

fn validate_request(request: &ReapRequest) -> Result<(), CoreError> {
    if request.entry.original_path.is_empty() || request.entry.surrogate_name.is_empty() {
        return Err(CoreError::invalid_argument(
            "Reap потребує original path і surrogate name.",
        ));
    }
    let destination_name = Path::new(&request.destination_path)
        .file_name()
        .and_then(|name| name.to_str());
    if destination_name != Some(request.entry.surrogate_name.as_str()) {
        return Err(CoreError::invalid_argument(
            "Surrogate name manifest не збігається з destination reap.",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{HashMap, HashSet};
    use std::panic::{catch_unwind, AssertUnwindSafe};
    use std::sync::{Arc, Mutex};
    use trashradar_domain::{
        candidate::{ByteSize, FsTimestamp},
        quarantine::{BatchId, QuarantineEntryId},
    };

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum CrashPhase {
        AfterJournal,
        AfterMove,
        BeforeConfirm,
    }

    struct CrashControl {
        phase: CrashPhase,
        crash_call: usize,
        inserts: Mutex<usize>,
        moves: Mutex<usize>,
        updates: Mutex<usize>,
    }

    impl CrashControl {
        fn hit(&self, phase: CrashPhase, counter: &Mutex<usize>) {
            let mut call = counter.lock().unwrap();
            *call += 1;
            if self.phase == phase && *call == self.crash_call {
                panic!("simulated hard process termination");
            }
        }
    }

    struct FakeManifest {
        entries: Arc<Mutex<HashMap<u64, QuarantineEntry>>>,
        crash: Arc<CrashControl>,
    }

    impl QuarantineManifest for FakeManifest {
        fn insert_entry(&self, entry: &QuarantineEntry) -> Result<(), CoreError> {
            self.entries
                .lock()
                .unwrap()
                .insert(entry.id.0, entry.clone());
            self.crash
                .hit(CrashPhase::AfterJournal, &self.crash.inserts);
            Ok(())
        }
        fn get_entry(&self, id: QuarantineEntryId) -> Result<Option<QuarantineEntry>, CoreError> {
            Ok(self.entries.lock().unwrap().get(&id.0).cloned())
        }
        fn list_entries(&self) -> Result<Vec<QuarantineEntry>, CoreError> {
            Ok(self.entries.lock().unwrap().values().cloned().collect())
        }
        fn update_status(
            &self,
            id: QuarantineEntryId,
            status: QuarantineStatus,
        ) -> Result<(), CoreError> {
            self.crash
                .hit(CrashPhase::BeforeConfirm, &self.crash.updates);
            self.entries.lock().unwrap().get_mut(&id.0).unwrap().status = status;
            Ok(())
        }
    }

    struct FakeFs {
        files: Arc<Mutex<HashSet<String>>>,
        crash: Arc<CrashControl>,
    }

    impl QuarantineFs for FakeFs {
        fn move_into_quarantine(
            &self,
            source: &str,
            destination: &str,
            _expected: FileIdentity,
            _roots: &[String],
        ) -> Result<(), CoreError> {
            let mut files = self.files.lock().unwrap();
            assert!(files.remove(source), "source exists exactly once");
            assert!(files.insert(destination.to_string()), "destination unique");
            drop(files);
            self.crash.hit(CrashPhase::AfterMove, &self.crash.moves);
            Ok(())
        }

        fn restore_from_quarantine(
            &self,
            surrogate_path: &str,
            destination_path: &str,
        ) -> Result<RestoreMove, CoreError> {
            let mut files = self.files.lock().unwrap();
            if files.contains(destination_path) {
                return Ok(RestoreMove::DestinationOccupied);
            }
            assert!(
                files.remove(surrogate_path),
                "surrogate exists exactly once"
            );
            assert!(files.insert(destination_path.to_string()));
            Ok(RestoreMove::Restored)
        }
    }

    fn request(id: u64) -> ReapRequest {
        let original = format!(r"C:\Users\Ada\Videos\clip-{id}.mp4");
        let surrogate = format!("{id:08}.bin");
        ReapRequest {
            entry: QuarantineEntry {
                id: QuarantineEntryId(id),
                batch_id: Some(BatchId(1)),
                original_path: original,
                surrogate_name: surrogate.clone(),
                size: ByteSize(100 + id),
                quarantined_at_unix: 1_750_000_000,
                expires_at_unix: 1_752_592_000,
                status: QuarantineStatus::InFlight,
            },
            destination_path: format!(r"C:\.trashradar\quarantine\{surrogate}"),
            expected_identity: FileIdentity {
                size: ByteSize(100 + id),
                modified_at: Some(FsTimestamp(id as i64)),
            },
        }
    }

    #[test]
    fn successful_reap_orders_journal_move_confirmation() {
        let requests = vec![request(1)];
        let files = Arc::new(Mutex::new(HashSet::from([requests[0]
            .entry
            .original_path
            .clone()])));
        let entries = Arc::new(Mutex::new(HashMap::new()));
        let crash = Arc::new(CrashControl {
            phase: CrashPhase::AfterMove,
            crash_call: usize::MAX,
            inserts: Mutex::new(0),
            moves: Mutex::new(0),
            updates: Mutex::new(0),
        });
        let fs = FakeFs {
            files: Arc::clone(&files),
            crash: Arc::clone(&crash),
        };
        let manifest = FakeManifest {
            entries: Arc::clone(&entries),
            crash,
        };
        let outcomes = TransactionalReaper::new(&fs, &manifest, &[])
            .reap_batch(BatchId(42), requests)
            .unwrap();
        assert_eq!(outcomes[0].entry.status, QuarantineStatus::Quarantined);
        assert_eq!(outcomes[0].entry.batch_id, Some(BatchId(42)));
        assert_eq!(
            entries.lock().unwrap()[&1].status,
            QuarantineStatus::Quarantined
        );
    }

    /// Пара FakeFs+FakeManifest без crash-інструментації для restore-тестів.
    fn restore_fixture(
        entry_status: QuarantineStatus,
        files: &[&str],
    ) -> (FakeFs, FakeManifest, RestoreRequest, String) {
        let quarantined = {
            let mut entry = request(1).entry;
            entry.status = entry_status;
            entry
        };
        let original = quarantined.original_path.clone();
        let surrogate_path = format!(r"C:\.trashradar\quarantine\{}", quarantined.surrogate_name);
        let crash = Arc::new(CrashControl {
            phase: CrashPhase::AfterMove,
            crash_call: usize::MAX,
            inserts: Mutex::new(0),
            moves: Mutex::new(0),
            updates: Mutex::new(0),
        });
        let fs = FakeFs {
            files: Arc::new(Mutex::new(
                files.iter().map(|f| f.to_string()).collect::<HashSet<_>>(),
            )),
            crash: Arc::clone(&crash),
        };
        let manifest = FakeManifest {
            entries: Arc::new(Mutex::new(HashMap::from([(1, quarantined.clone())]))),
            crash,
        };
        let request = RestoreRequest {
            entry_id: quarantined.id,
            surrogate_path,
        };
        (fs, manifest, request, original)
    }

    /// DoD T-080: відновлення повертає файл на оригінальний шлях; статус → Restored.
    #[test]
    fn restore_returns_file_to_original_path() {
        let (fs, manifest, request, original) = restore_fixture(QuarantineStatus::Quarantined, &[]);
        {
            fs.files
                .lock()
                .unwrap()
                .insert(request.surrogate_path.clone());
        }
        let outcome = QuarantineRestorer::new(&fs, &manifest)
            .restore_one(request.clone())
            .unwrap();
        assert_eq!(outcome.restored_path, original);
        assert!(!outcome.used_suffix);
        assert_eq!(outcome.entry.status, QuarantineStatus::Restored);
        assert_eq!(
            manifest.entries.lock().unwrap()[&1].status,
            QuarantineStatus::Restored
        );
        let files = fs.files.lock().unwrap();
        assert!(files.contains(&original));
        assert!(!files.contains(&request.surrogate_path));
    }

    /// DoD T-080: зайнятий шлях → суфікс; чужий файл на оригінальному шляху
    /// не перезаписаний.
    #[test]
    fn restore_occupied_path_uses_suffix_without_overwrite() {
        let (fs, manifest, request, original) = restore_fixture(QuarantineStatus::Quarantined, &[]);
        {
            let mut files = fs.files.lock().unwrap();
            files.insert(request.surrogate_path.clone());
            files.insert(original.clone()); // хтось уже зайняв оригінальний шлях
        }
        let outcome = QuarantineRestorer::new(&fs, &manifest)
            .restore_one(request)
            .unwrap();
        assert!(outcome.used_suffix);
        assert_ne!(outcome.restored_path, original);
        assert!(outcome.restored_path.contains("(відновлено)"));
        let files = fs.files.lock().unwrap();
        assert!(files.contains(&original), "чужий файл лишився недоторканим");
        assert!(files.contains(&outcome.restored_path));
        assert_eq!(
            manifest.entries.lock().unwrap()[&1].status,
            QuarantineStatus::Restored
        );
    }

    /// Відновлювати можна лише quarantined-записи: in_flight/restored — відмова.
    #[test]
    fn restore_rejects_non_quarantined_entry() {
        use trashradar_domain::error::ErrorCode;
        let (fs, manifest, request, _original) = restore_fixture(QuarantineStatus::Restored, &[]);
        let error = QuarantineRestorer::new(&fs, &manifest)
            .restore_one(request)
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidArgument);
        assert!(error.message.contains("quarantined"));
    }

    #[test]
    fn undo_batch_restores_all_matching_entries_in_one_call() {
        let mut entries = [request(1).entry, request(2).entry];
        for entry in &mut entries {
            entry.batch_id = Some(BatchId(77));
            entry.status = QuarantineStatus::Quarantined;
        }
        let paths = |entry: &QuarantineEntry| {
            format!(r"C:\.trashradar\quarantine\{}", entry.surrogate_name)
        };
        let crash = Arc::new(CrashControl {
            phase: CrashPhase::AfterMove,
            crash_call: usize::MAX,
            inserts: Mutex::new(0),
            moves: Mutex::new(0),
            updates: Mutex::new(0),
        });
        let fs = FakeFs {
            files: Arc::new(Mutex::new(entries.iter().map(paths).collect())),
            crash: Arc::clone(&crash),
        };
        let manifest = FakeManifest {
            entries: Arc::new(Mutex::new(
                entries.iter().map(|e| (e.id.0, e.clone())).collect(),
            )),
            crash,
        };
        let outcomes = QuarantineRestorer::new(&fs, &manifest)
            .undo_batch(BatchId(77), |entry| Ok(paths(entry)))
            .unwrap();
        assert_eq!(outcomes.len(), 2);
        assert!(outcomes
            .iter()
            .all(|outcome| outcome.entry.status == QuarantineStatus::Restored));
    }

    #[test]
    fn stress_kill_mid_batch_never_loses_or_duplicates_file() {
        const FILES: u64 = 64;
        const CRASH_CALL: usize = 32;
        for phase in [
            CrashPhase::AfterJournal,
            CrashPhase::AfterMove,
            CrashPhase::BeforeConfirm,
        ] {
            let requests: Vec<_> = (0..FILES).map(request).collect();
            let originals: HashSet<_> = requests
                .iter()
                .map(|r| r.entry.original_path.clone())
                .collect();
            let files = Arc::new(Mutex::new(originals));
            let entries = Arc::new(Mutex::new(HashMap::new()));
            let crash = Arc::new(CrashControl {
                phase,
                crash_call: CRASH_CALL,
                inserts: Mutex::new(0),
                moves: Mutex::new(0),
                updates: Mutex::new(0),
            });
            let fs = FakeFs {
                files: Arc::clone(&files),
                crash: Arc::clone(&crash),
            };
            let manifest = FakeManifest {
                entries: Arc::clone(&entries),
                crash,
            };
            let reaper = TransactionalReaper::new(&fs, &manifest, &[]);
            assert!(catch_unwind(AssertUnwindSafe(|| {
                reaper.reap_batch(BatchId(1), requests.clone())
            }))
            .is_err());

            let physical = files.lock().unwrap();
            for req in &requests {
                let at_source = physical.contains(&req.entry.original_path);
                let at_destination = physical.contains(&req.destination_path);
                assert_ne!(
                    at_source, at_destination,
                    "id {} must have exactly one copy",
                    req.entry.id.0
                );
            }
            for entry in entries.lock().unwrap().values() {
                assert!(matches!(
                    entry.status,
                    QuarantineStatus::InFlight | QuarantineStatus::Quarantined
                ));
            }
        }
    }
}
