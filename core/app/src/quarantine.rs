//! Application use cases Quarantine: транзакційний reap (T-079) і
//! restore на оригінальний шлях із суфіксом при конфлікті (T-080) —
//! architecture.md §7.3.

use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;

use trashradar_domain::{
    error::CoreError,
    quarantine::{
        restore_destination, AuditActor, AuditOperation, BatchId, DestructiveAuditEvent,
        FileIdentity, QuarantineEntry, QuarantineEntryId, QuarantineStatus,
        RESTORE_SUFFIX_MAX_ATTEMPTS,
    },
};

use crate::ports::{QuarantineFs, QuarantineManifest, RecoveryLocation, RestoreMove};
use crate::workers::{JobHandle, JobPriority, WorkerPool};

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
        confirm_with_audit(
            self.manifest,
            &request.entry,
            QuarantineStatus::Quarantined,
            AuditOperation::Reap,
            AuditActor::User,
        )?;
        request.entry.status = QuarantineStatus::Quarantined;
        Ok(ReapOutcome {
            entry: request.entry,
            destination_path: request.destination_path,
        })
    }

    /// Фазовий батч (T-154: reap 1 000 файлів < 2 с, §15): валідація всіх →
    /// один durable-запис усіх in_flight → послідовні атомарні move → одне
    /// підтвердження + аудит. Семантика crash-recovery T-084 незмінна: після
    /// аварії у будь-якій фазі кожен запис журналу reconcile докочує
    /// (файл уже в карантині) або відкочує (файл ще на місці) звіркою з
    /// реальністю — жоден файл не губиться і не дублюється.
    ///
    /// Перша помилка move зупиняє нові move; вже переміщені файли
    /// підтверджуються, незаймані записи відкочуються з журналу (їхній move
    /// не стартував → файл гарантовано на місці), а запис невдалого move
    /// лишається in_flight до reconcile — як і в `reap_one`.
    pub fn reap_batch(
        &self,
        batch_id: BatchId,
        mut requests: Vec<ReapRequest>,
    ) -> Result<Vec<ReapOutcome>, CoreError> {
        for request in &mut requests {
            validate_request(request)?;
            request.entry.batch_id = Some(batch_id);
            request.entry.status = QuarantineStatus::InFlight;
        }

        let entries: Vec<QuarantineEntry> = requests
            .iter()
            .map(|request| request.entry.clone())
            .collect();
        self.manifest.insert_entries(&entries)?;

        let mut moved = 0usize;
        let mut move_error = None;
        for request in &requests {
            match self.filesystem.move_into_quarantine(
                &request.entry.original_path,
                &request.destination_path,
                request.expected_identity,
                self.trashradar_roots,
            ) {
                Ok(()) => moved += 1,
                Err(error) => {
                    move_error = Some(error);
                    break;
                }
            }
        }

        self.confirm_reaped(&requests[..moved])?;
        if let Some(error) = move_error {
            // Незаймані запити (за невдалим): move не стартував, файл на
            // місці — запис можна прибрати одразу, не чекаючи reconcile.
            for request in &requests[moved + 1..] {
                self.manifest.remove_entry(request.entry.id)?;
            }
            return Err(error);
        }

        Ok(requests
            .into_iter()
            .map(|mut request| {
                request.entry.status = QuarantineStatus::Quarantined;
                ReapOutcome {
                    entry: request.entry,
                    destination_path: request.destination_path,
                }
            })
            .collect())
    }

    /// Підтвердження + аудит переміщених файлів одним durable-записом.
    fn confirm_reaped(&self, requests: &[ReapRequest]) -> Result<(), CoreError> {
        if requests.is_empty() {
            return Ok(());
        }
        let confirmations: Vec<_> = requests
            .iter()
            .map(|request| {
                (
                    request.entry.id,
                    QuarantineStatus::Quarantined,
                    DestructiveAuditEvent {
                        entry_id: request.entry.id,
                        batch_id: request.entry.batch_id,
                        operation: AuditOperation::Reap,
                        actor: AuditActor::User,
                        original_path: request.entry.original_path.clone(),
                        size: request.entry.size,
                    },
                )
            })
            .collect();
        self.manifest.confirm_batch_with_audit(&confirmations)
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

        confirm_with_audit(
            self.manifest,
            &entry,
            QuarantineStatus::Restored,
            AuditOperation::Restore,
            AuditActor::User,
        )?;
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SweepResult {
    pub purged: Vec<QuarantineEntry>,
    pub purged_bytes: u64,
    pub held_bytes: u64,
    pub threshold_exceeded: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManualPurgeSelection {
    Entries(Vec<QuarantineEntryId>),
    All,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManualPurgeResult {
    pub purged: Vec<QuarantineEntry>,
    pub purged_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RecoveryResult {
    pub completed: Vec<QuarantineEntryId>,
    pub rolled_back: Vec<QuarantineEntryId>,
}

pub struct QuarantineRecovery<'a, F: QuarantineFs, M: QuarantineManifest> {
    filesystem: &'a F,
    manifest: &'a M,
}

impl<'a, F: QuarantineFs, M: QuarantineManifest> QuarantineRecovery<'a, F, M> {
    pub fn new(filesystem: &'a F, manifest: &'a M) -> Self {
        Self {
            filesystem,
            manifest,
        }
    }

    pub fn reconcile(
        &self,
        mut surrogate_path: impl FnMut(&QuarantineEntry) -> Result<String, CoreError>,
    ) -> Result<RecoveryResult, CoreError> {
        let mut in_flight: Vec<_> = self
            .manifest
            .list_entries()?
            .into_iter()
            .filter(|entry| entry.status == QuarantineStatus::InFlight)
            .collect();
        in_flight.sort_by_key(|entry| entry.id.0);
        let mut result = RecoveryResult::default();
        for entry in in_flight {
            let surrogate = surrogate_path(&entry)?;
            match self
                .filesystem
                .recovery_location(&entry.original_path, &surrogate)?
            {
                RecoveryLocation::SourceOnly => {
                    self.manifest.remove_entry(entry.id)?;
                    result.rolled_back.push(entry.id);
                }
                RecoveryLocation::QuarantineOnly => {
                    confirm_with_audit(
                        self.manifest,
                        &entry,
                        QuarantineStatus::Quarantined,
                        AuditOperation::Reap,
                        AuditActor::Recovery,
                    )?;
                    result.completed.push(entry.id);
                }
                RecoveryLocation::Both => {
                    return Err(CoreError::internal(format!(
                        "Crash recovery виявив дві копії entry {} — потрібне ручне втручання.",
                        entry.id.0
                    )));
                }
                RecoveryLocation::Missing => {
                    return Err(CoreError::internal(format!(
                        "Crash recovery не знайшов жодної копії entry {} — потрібне ручне втручання.",
                        entry.id.0
                    )));
                }
            }
        }
        Ok(result)
    }
}

pub struct ManualPurger<'a, F: QuarantineFs, M: QuarantineManifest> {
    filesystem: &'a F,
    manifest: &'a M,
}

impl<'a, F: QuarantineFs, M: QuarantineManifest> ManualPurger<'a, F, M> {
    pub fn new(filesystem: &'a F, manifest: &'a M) -> Self {
        Self {
            filesystem,
            manifest,
        }
    }

    pub fn purge(
        &self,
        selection: ManualPurgeSelection,
        mut surrogate_path: impl FnMut(&QuarantineEntry) -> Result<String, CoreError>,
    ) -> Result<ManualPurgeResult, CoreError> {
        let entries = self.manifest.list_entries()?;
        let selected_ids = match selection {
            ManualPurgeSelection::Entries(ids) => {
                if ids.is_empty() {
                    return Err(CoreError::invalid_argument(
                        "Вибірковий purge потребує хоча б один entry ID.",
                    ));
                }
                Some(ids.into_iter().collect::<HashSet<_>>())
            }
            ManualPurgeSelection::All => None,
        };
        let mut targets: Vec<_> = entries
            .into_iter()
            .filter(|entry| {
                entry.status == QuarantineStatus::Quarantined
                    && selected_ids
                        .as_ref()
                        .is_none_or(|ids| ids.contains(&entry.id))
            })
            .collect();
        if let Some(ids) = &selected_ids {
            if targets.len() != ids.len() {
                return Err(CoreError::invalid_argument(
                    "Ручний purge дозволений лише для наявних quarantined-записів.",
                ));
            }
        }
        targets.sort_by_key(|entry| entry.id.0);
        let mut purged = Vec::with_capacity(targets.len());
        let mut purged_bytes = 0u64;
        for mut entry in targets {
            self.filesystem
                .purge_from_quarantine(&surrogate_path(&entry)?)?;
            confirm_with_audit(
                self.manifest,
                &entry,
                QuarantineStatus::Purged,
                AuditOperation::PurgeManual,
                AuditActor::User,
            )?;
            purged_bytes = purged_bytes.saturating_add(entry.size.0);
            entry.status = QuarantineStatus::Purged;
            purged.push(entry);
        }
        Ok(ManualPurgeResult {
            purged,
            purged_bytes,
        })
    }
}

/// Фоновий TTL-sweeper Quarantine (T-082).
pub struct QuarantineSweeper<'a, F: QuarantineFs, M: QuarantineManifest> {
    filesystem: &'a F,
    manifest: &'a M,
}

impl<'a, F: QuarantineFs, M: QuarantineManifest> QuarantineSweeper<'a, F, M> {
    pub fn new(filesystem: &'a F, manifest: &'a M) -> Self {
        Self {
            filesystem,
            manifest,
        }
    }

    pub fn sweep_once(
        &self,
        now_unix: i64,
        warning_threshold_bytes: u64,
        mut surrogate_path: impl FnMut(&QuarantineEntry) -> Result<String, CoreError>,
    ) -> Result<SweepResult, CoreError> {
        let entries = self.manifest.list_entries()?;
        let mut purged = Vec::new();
        let mut purged_bytes = 0u64;
        for mut entry in entries
            .iter()
            .filter(|entry| {
                entry.status == QuarantineStatus::Quarantined && entry.expires_at_unix <= now_unix
            })
            .cloned()
        {
            self.filesystem
                .purge_from_quarantine(&surrogate_path(&entry)?)?;
            confirm_with_audit(
                self.manifest,
                &entry,
                QuarantineStatus::Purged,
                AuditOperation::PurgeTtl,
                AuditActor::Sweeper,
            )?;
            purged_bytes = purged_bytes.saturating_add(entry.size.0);
            entry.status = QuarantineStatus::Purged;
            purged.push(entry);
        }
        let held_bytes = entries
            .iter()
            .filter(|entry| {
                entry.status == QuarantineStatus::Quarantined && entry.expires_at_unix > now_unix
            })
            .fold(0u64, |total, entry| total.saturating_add(entry.size.0));
        Ok(SweepResult {
            purged,
            purged_bytes,
            held_bytes,
            threshold_exceeded: held_bytes > warning_threshold_bytes,
        })
    }
}

/// Поставити один плановий sweep у фонову чергу T-008.
pub fn spawn_quarantine_sweep<F, M, P, C>(
    pool: &WorkerPool,
    filesystem: Arc<F>,
    manifest: Arc<M>,
    now_unix: i64,
    warning_threshold_bytes: u64,
    surrogate_path: P,
    on_complete: C,
) -> JobHandle
where
    F: QuarantineFs + Send + Sync + 'static,
    M: QuarantineManifest + Send + Sync + 'static,
    P: Fn(&QuarantineEntry) -> Result<String, CoreError> + Send + Sync + 'static,
    C: FnOnce(Result<SweepResult, CoreError>) + Send + 'static,
{
    pool.submit(JobPriority::Background, move |cancel| {
        if cancel.is_cancelled() {
            return;
        }
        let result = QuarantineSweeper::new(filesystem.as_ref(), manifest.as_ref()).sweep_once(
            now_unix,
            warning_threshold_bytes,
            surrogate_path,
        );
        on_complete(result);
    })
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

fn confirm_with_audit<M: QuarantineManifest>(
    manifest: &M,
    entry: &QuarantineEntry,
    status: QuarantineStatus,
    operation: AuditOperation,
    actor: AuditActor,
) -> Result<(), CoreError> {
    manifest.confirm_with_audit(
        entry.id,
        status,
        &DestructiveAuditEvent {
            entry_id: entry.id,
            batch_id: entry.batch_id,
            operation,
            actor,
            original_path: entry.original_path.clone(),
            size: entry.size,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{HashMap, HashSet};
    use std::panic::{catch_unwind, AssertUnwindSafe};
    use std::sync::{Arc, Mutex};
    use trashradar_domain::{
        candidate::{ByteSize, FsTimestamp},
        quarantine::{BatchId, DestructiveAuditRecord, QuarantineEntryId},
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
        audits: Mutex<Vec<DestructiveAuditEvent>>,
    }

    impl CrashControl {
        fn hit(&self, phase: CrashPhase, counter: &Mutex<usize>) {
            // Панічна «аварія» отруює лок лічильника; «рестарт» (reconcile
            // у стрес-тесті) продовжує роботу, як новий процес.
            let mut call = counter
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
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
        fn remove_entry(&self, id: QuarantineEntryId) -> Result<(), CoreError> {
            self.entries.lock().unwrap().remove(&id.0);
            Ok(())
        }
        fn append_audit(&self, event: &DestructiveAuditEvent) -> Result<(), CoreError> {
            self.crash.audits.lock().unwrap().push(event.clone());
            Ok(())
        }
        fn list_audit(&self) -> Result<Vec<DestructiveAuditRecord>, CoreError> {
            Ok(self
                .crash
                .audits
                .lock()
                .unwrap()
                .iter()
                .cloned()
                .enumerate()
                .map(|(index, event)| DestructiveAuditRecord {
                    sequence: index as u64 + 1,
                    event,
                    occurred_at_unix: 1_800_000_000,
                })
                .collect())
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

        fn purge_from_quarantine(&self, surrogate_path: &str) -> Result<(), CoreError> {
            if self.files.lock().unwrap().remove(surrogate_path) {
                Ok(())
            } else {
                Err(CoreError::io("surrogate missing"))
            }
        }

        fn recovery_location(
            &self,
            source_path: &str,
            surrogate_path: &str,
        ) -> Result<RecoveryLocation, CoreError> {
            let files = self.files.lock().unwrap();
            Ok(
                match (files.contains(source_path), files.contains(surrogate_path)) {
                    (true, false) => RecoveryLocation::SourceOnly,
                    (false, true) => RecoveryLocation::QuarantineOnly,
                    (true, true) => RecoveryLocation::Both,
                    (false, false) => RecoveryLocation::Missing,
                },
            )
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
            audits: Mutex::new(Vec::new()),
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
        let audit = manifest.list_audit().unwrap();
        assert_eq!(audit[0].event.operation, AuditOperation::Reap);
        assert_eq!(audit[0].event.actor, AuditActor::User);
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
            audits: Mutex::new(Vec::new()),
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
        assert_eq!(
            manifest.list_audit().unwrap()[0].event.operation,
            AuditOperation::Restore
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
            audits: Mutex::new(Vec::new()),
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
    fn sweeper_purges_expired_and_reports_held_threshold() {
        let now = 1_800_000_000;
        let mut expired = request(1).entry;
        expired.status = QuarantineStatus::Quarantined;
        expired.expires_at_unix = now;
        let mut future = request(2).entry;
        future.status = QuarantineStatus::Quarantined;
        future.expires_at_unix = now + 60;
        let path = |entry: &QuarantineEntry| {
            format!(r"C:\.trashradar\quarantine\{}", entry.surrogate_name)
        };
        let crash = Arc::new(CrashControl {
            phase: CrashPhase::AfterMove,
            crash_call: usize::MAX,
            inserts: Mutex::new(0),
            moves: Mutex::new(0),
            updates: Mutex::new(0),
            audits: Mutex::new(Vec::new()),
        });
        let fs = FakeFs {
            files: Arc::new(Mutex::new(
                [path(&expired), path(&future)].into_iter().collect(),
            )),
            crash: Arc::clone(&crash),
        };
        let manifest = FakeManifest {
            entries: Arc::new(Mutex::new(HashMap::from([
                (expired.id.0, expired.clone()),
                (future.id.0, future.clone()),
            ]))),
            crash,
        };
        let result = QuarantineSweeper::new(&fs, &manifest)
            .sweep_once(now, 1, |entry| Ok(path(entry)))
            .unwrap();
        assert_eq!(result.purged.len(), 1);
        assert_eq!(result.purged_bytes, expired.size.0);
        assert_eq!(result.held_bytes, future.size.0);
        assert!(result.threshold_exceeded);
        let audit = manifest.list_audit().unwrap();
        assert_eq!(audit[0].event.operation, AuditOperation::PurgeTtl);
        assert_eq!(audit[0].event.actor, AuditActor::Sweeper);
        assert_eq!(
            manifest.entries.lock().unwrap()[&1].status,
            QuarantineStatus::Purged
        );
        assert!(fs.files.lock().unwrap().contains(&path(&future)));
    }

    #[test]
    fn manual_purge_supports_selection_then_empty_all() {
        let mut entries = [request(1).entry, request(2).entry, request(3).entry];
        for entry in &mut entries {
            entry.status = QuarantineStatus::Quarantined;
        }
        let path = |entry: &QuarantineEntry| {
            format!(r"C:\.trashradar\quarantine\{}", entry.surrogate_name)
        };
        let crash = Arc::new(CrashControl {
            phase: CrashPhase::AfterMove,
            crash_call: usize::MAX,
            inserts: Mutex::new(0),
            moves: Mutex::new(0),
            updates: Mutex::new(0),
            audits: Mutex::new(Vec::new()),
        });
        let fs = FakeFs {
            files: Arc::new(Mutex::new(entries.iter().map(path).collect())),
            crash: Arc::clone(&crash),
        };
        let manifest = FakeManifest {
            entries: Arc::new(Mutex::new(
                entries.iter().map(|e| (e.id.0, e.clone())).collect(),
            )),
            crash,
        };
        let purger = ManualPurger::new(&fs, &manifest);
        let selected = purger
            .purge(
                ManualPurgeSelection::Entries(vec![QuarantineEntryId(2)]),
                |entry| Ok(path(entry)),
            )
            .unwrap();
        assert_eq!(
            selected.purged.iter().map(|e| e.id.0).collect::<Vec<_>>(),
            vec![2]
        );
        assert_eq!(selected.purged_bytes, entries[1].size.0);
        assert_eq!(
            manifest.list_audit().unwrap()[0].event.operation,
            AuditOperation::PurgeManual
        );

        let all = purger
            .purge(ManualPurgeSelection::All, |entry| Ok(path(entry)))
            .unwrap();
        assert_eq!(
            all.purged.iter().map(|e| e.id.0).collect::<Vec<_>>(),
            vec![1, 3]
        );
        assert!(fs.files.lock().unwrap().is_empty());
        assert!(manifest
            .entries
            .lock()
            .unwrap()
            .values()
            .all(|entry| entry.status == QuarantineStatus::Purged));
    }

    #[test]
    fn manual_purge_rejects_missing_or_non_quarantined_selection() {
        use trashradar_domain::error::ErrorCode;
        let (fs, manifest, _, _) = restore_fixture(QuarantineStatus::Restored, &[]);
        let purger = ManualPurger::new(&fs, &manifest);
        for ids in [
            vec![],
            vec![QuarantineEntryId(1)],
            vec![QuarantineEntryId(99)],
        ] {
            let error = purger
                .purge(ManualPurgeSelection::Entries(ids), |_| Ok(String::new()))
                .unwrap_err();
            assert_eq!(error.code, ErrorCode::InvalidArgument);
        }
    }

    #[test]
    fn crash_recovery_rolls_back_before_move_and_completes_after_move() {
        let source_only = request(1);
        let quarantine_only = request(2);
        let entries = [source_only.entry.clone(), quarantine_only.entry.clone()];
        let crash = Arc::new(CrashControl {
            phase: CrashPhase::AfterMove,
            crash_call: usize::MAX,
            inserts: Mutex::new(0),
            moves: Mutex::new(0),
            updates: Mutex::new(0),
            audits: Mutex::new(Vec::new()),
        });
        let fs = FakeFs {
            files: Arc::new(Mutex::new(HashSet::from([
                source_only.entry.original_path.clone(),
                quarantine_only.destination_path.clone(),
            ]))),
            crash: Arc::clone(&crash),
        };
        let manifest = FakeManifest {
            entries: Arc::new(Mutex::new(
                entries.iter().map(|e| (e.id.0, e.clone())).collect(),
            )),
            crash,
        };
        let result = QuarantineRecovery::new(&fs, &manifest)
            .reconcile(|entry| {
                Ok(format!(
                    r"C:\.trashradar\quarantine\{}",
                    entry.surrogate_name
                ))
            })
            .unwrap();
        assert_eq!(result.rolled_back, vec![QuarantineEntryId(1)]);
        assert_eq!(result.completed, vec![QuarantineEntryId(2)]);
        let stored = manifest.entries.lock().unwrap();
        assert!(!stored.contains_key(&1));
        assert_eq!(stored[&2].status, QuarantineStatus::Quarantined);
    }

    #[test]
    fn crash_recovery_fails_closed_for_duplicate_or_missing_file() {
        use trashradar_domain::error::ErrorCode;
        for files in [
            vec![request(1).entry.original_path, request(1).destination_path],
            vec![],
        ] {
            let entry = request(1).entry;
            let crash = Arc::new(CrashControl {
                phase: CrashPhase::AfterMove,
                crash_call: usize::MAX,
                inserts: Mutex::new(0),
                moves: Mutex::new(0),
                updates: Mutex::new(0),
                audits: Mutex::new(Vec::new()),
            });
            let fs = FakeFs {
                files: Arc::new(Mutex::new(files.into_iter().collect())),
                crash: Arc::clone(&crash),
            };
            let manifest = FakeManifest {
                entries: Arc::new(Mutex::new(HashMap::from([(1, entry)]))),
                crash,
            };
            let error = QuarantineRecovery::new(&fs, &manifest)
                .reconcile(|entry| {
                    Ok(format!(
                        r"C:\.trashradar\quarantine\{}",
                        entry.surrogate_name
                    ))
                })
                .unwrap_err();
            assert_eq!(error.code, ErrorCode::Internal);
            assert_eq!(
                manifest.entries.lock().unwrap()[&1].status,
                QuarantineStatus::InFlight
            );
        }
    }

    /// Обгортка manifest: рахує батч-виклики порту (T-154 — фазове
    /// батчування не повинно тихо відкотитися до per-file комітів).
    struct BatchCountingManifest<'m> {
        inner: &'m FakeManifest,
        batch_inserts: Mutex<usize>,
        batch_confirms: Mutex<usize>,
    }

    impl QuarantineManifest for BatchCountingManifest<'_> {
        fn insert_entry(&self, entry: &QuarantineEntry) -> Result<(), CoreError> {
            self.inner.insert_entry(entry)
        }
        fn insert_entries(&self, entries: &[QuarantineEntry]) -> Result<(), CoreError> {
            *self.batch_inserts.lock().unwrap() += 1;
            self.inner.insert_entries(entries)
        }
        fn get_entry(&self, id: QuarantineEntryId) -> Result<Option<QuarantineEntry>, CoreError> {
            self.inner.get_entry(id)
        }
        fn list_entries(&self) -> Result<Vec<QuarantineEntry>, CoreError> {
            self.inner.list_entries()
        }
        fn remove_entry(&self, id: QuarantineEntryId) -> Result<(), CoreError> {
            self.inner.remove_entry(id)
        }
        fn append_audit(&self, event: &DestructiveAuditEvent) -> Result<(), CoreError> {
            self.inner.append_audit(event)
        }
        fn list_audit(&self) -> Result<Vec<DestructiveAuditRecord>, CoreError> {
            self.inner.list_audit()
        }
        fn confirm_batch_with_audit(
            &self,
            confirmations: &[(QuarantineEntryId, QuarantineStatus, DestructiveAuditEvent)],
        ) -> Result<(), CoreError> {
            *self.batch_confirms.lock().unwrap() += 1;
            self.inner.confirm_batch_with_audit(confirmations)
        }
        fn update_status(
            &self,
            id: QuarantineEntryId,
            status: QuarantineStatus,
        ) -> Result<(), CoreError> {
            self.inner.update_status(id, status)
        }
    }

    /// Обгортка FS: n-й move повертає помилку (не панікує) без переміщення.
    struct FailingMoveFs<'f> {
        inner: &'f FakeFs,
        fail_move_call: usize,
        moves: Mutex<usize>,
    }

    impl QuarantineFs for FailingMoveFs<'_> {
        fn move_into_quarantine(
            &self,
            source: &str,
            destination: &str,
            expected: FileIdentity,
            roots: &[String],
        ) -> Result<(), CoreError> {
            let mut calls = self.moves.lock().unwrap();
            *calls += 1;
            if *calls == self.fail_move_call {
                return Err(CoreError::io("simulated move failure"));
            }
            drop(calls);
            self.inner
                .move_into_quarantine(source, destination, expected, roots)
        }
        fn restore_from_quarantine(
            &self,
            surrogate_path: &str,
            destination_path: &str,
        ) -> Result<RestoreMove, CoreError> {
            self.inner
                .restore_from_quarantine(surrogate_path, destination_path)
        }
        fn purge_from_quarantine(&self, surrogate_path: &str) -> Result<(), CoreError> {
            self.inner.purge_from_quarantine(surrogate_path)
        }
        fn recovery_location(
            &self,
            source_path: &str,
            surrogate_path: &str,
        ) -> Result<RecoveryLocation, CoreError> {
            self.inner.recovery_location(source_path, surrogate_path)
        }
    }

    fn inert_crash() -> Arc<CrashControl> {
        Arc::new(CrashControl {
            phase: CrashPhase::AfterMove,
            crash_call: usize::MAX,
            inserts: Mutex::new(0),
            moves: Mutex::new(0),
            updates: Mutex::new(0),
            audits: Mutex::new(Vec::new()),
        })
    }

    /// T-154: успішний батч робить рівно один батч-запис журналу і рівно
    /// одне батч-підтвердження — не по коміту на файл.
    #[test]
    fn reap_batch_uses_single_journal_and_confirm_batches() {
        let requests: Vec<_> = (1..=3).map(request).collect();
        let files = Arc::new(Mutex::new(
            requests
                .iter()
                .map(|r| r.entry.original_path.clone())
                .collect::<HashSet<_>>(),
        ));
        let crash = inert_crash();
        let fs = FakeFs {
            files: Arc::clone(&files),
            crash: Arc::clone(&crash),
        };
        let inner = FakeManifest {
            entries: Arc::new(Mutex::new(HashMap::new())),
            crash,
        };
        let manifest = BatchCountingManifest {
            inner: &inner,
            batch_inserts: Mutex::new(0),
            batch_confirms: Mutex::new(0),
        };
        let outcomes = TransactionalReaper::new(&fs, &manifest, &[])
            .reap_batch(BatchId(9), requests)
            .unwrap();
        assert_eq!(outcomes.len(), 3);
        assert!(outcomes
            .iter()
            .all(|o| o.entry.status == QuarantineStatus::Quarantined));
        assert_eq!(*manifest.batch_inserts.lock().unwrap(), 1);
        assert_eq!(*manifest.batch_confirms.lock().unwrap(), 1);
        let audit = inner.list_audit().unwrap();
        assert_eq!(audit.len(), 3);
        assert!(audit
            .iter()
            .all(|record| record.event.operation == AuditOperation::Reap
                && record.event.actor == AuditActor::User));
    }

    /// T-154: помилка move посеред батчу — переміщені підтверджені з аудитом,
    /// незаймані відкочені з журналу, невдалий лишається in_flight і
    /// розв'язується reconcile T-084 (файл на місці → rollback).
    #[test]
    fn reap_batch_move_error_confirms_moved_rolls_back_untouched() {
        let requests: Vec<_> = (1..=4).map(request).collect();
        let files = Arc::new(Mutex::new(
            requests
                .iter()
                .map(|r| r.entry.original_path.clone())
                .collect::<HashSet<_>>(),
        ));
        let crash = inert_crash();
        let inner_fs = FakeFs {
            files: Arc::clone(&files),
            crash: Arc::clone(&crash),
        };
        let fs = FailingMoveFs {
            inner: &inner_fs,
            fail_move_call: 2,
            moves: Mutex::new(0),
        };
        let manifest = FakeManifest {
            entries: Arc::new(Mutex::new(HashMap::new())),
            crash,
        };
        let error = TransactionalReaper::new(&fs, &manifest, &[])
            .reap_batch(BatchId(9), requests.clone())
            .unwrap_err();
        assert!(error.message.contains("simulated move failure"));

        {
            let entries = manifest.entries.lock().unwrap();
            assert_eq!(entries[&1].status, QuarantineStatus::Quarantined);
            assert_eq!(entries[&2].status, QuarantineStatus::InFlight);
            assert!(!entries.contains_key(&3), "untouched rolled back");
            assert!(!entries.contains_key(&4), "untouched rolled back");
        }
        let audit = manifest.list_audit().unwrap();
        assert_eq!(audit.len(), 1, "аудит лише для підтвердженого");
        assert_eq!(audit[0].event.entry_id, QuarantineEntryId(1));
        {
            let physical = files.lock().unwrap();
            assert!(physical.contains(&requests[0].destination_path));
            for req in &requests[1..] {
                assert!(physical.contains(&req.entry.original_path));
            }
        }

        // Reconcile докочує залишок: невдалий move → файл на місці → rollback.
        let result = QuarantineRecovery::new(&inner_fs, &manifest)
            .reconcile(|entry| {
                Ok(requests
                    .iter()
                    .find(|r| r.entry.id == entry.id)
                    .unwrap()
                    .destination_path
                    .clone())
            })
            .unwrap();
        assert_eq!(result.rolled_back, vec![QuarantineEntryId(2)]);
        assert!(result.completed.is_empty());
        let entries = manifest.entries.lock().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[&1].status, QuarantineStatus::Quarantined);
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
                audits: Mutex::new(Vec::new()),
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

            {
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

            // T-084/T-154: рестарт після аварії — reconcile розв'язує КОЖЕН
            // in_flight (докочує чи відкочує), кінцевий стан консистентний.
            let in_flight_before = entries
                .lock()
                .unwrap()
                .values()
                .filter(|entry| entry.status == QuarantineStatus::InFlight)
                .count();
            let destination_of = |entry: &QuarantineEntry| {
                requests
                    .iter()
                    .find(|req| req.entry.id == entry.id)
                    .unwrap()
                    .destination_path
                    .clone()
            };
            let result = QuarantineRecovery::new(&fs, &manifest)
                .reconcile(|entry| Ok(destination_of(entry)))
                .unwrap();
            assert_eq!(
                result.completed.len() + result.rolled_back.len(),
                in_flight_before,
                "reconcile має розв'язати кожен in_flight"
            );
            let physical = files.lock().unwrap();
            let stored = entries.lock().unwrap();
            for req in &requests {
                match stored.get(&req.entry.id.0) {
                    Some(entry) => {
                        assert_eq!(entry.status, QuarantineStatus::Quarantined);
                        assert!(physical.contains(&req.destination_path));
                    }
                    None => assert!(physical.contains(&req.entry.original_path)),
                }
            }
        }
    }
}
