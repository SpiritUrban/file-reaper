//! Application use case транзакційного reap (T-079, architecture.md §7.3).

use std::path::Path;

use trashradar_domain::{
    error::CoreError,
    quarantine::{FileIdentity, QuarantineEntry, QuarantineStatus},
};

use crate::ports::{QuarantineFs, QuarantineManifest};

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
    pub fn reap_batch(&self, requests: Vec<ReapRequest>) -> Result<Vec<ReapOutcome>, CoreError> {
        requests
            .into_iter()
            .map(|request| self.reap_one(request))
            .collect()
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
            .reap_batch(requests)
            .unwrap();
        assert_eq!(outcomes[0].entry.status, QuarantineStatus::Quarantined);
        assert_eq!(
            entries.lock().unwrap()[&1].status,
            QuarantineStatus::Quarantined
        );
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
            assert!(
                catch_unwind(AssertUnwindSafe(|| reaper.reap_batch(requests.clone()))).is_err()
            );

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
