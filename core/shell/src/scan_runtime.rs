//! Рантайм сесії скану в shell (T-033): IPC start/stop + прогрес-події.
//!
//! Бізнес-логіка сесії — `trashradar_app::scan_control`; тут лише
//! Tauri State, фоновий потік і адаптери MFT/walk.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Runtime, State};
use trashradar_app::detectors::{
    DetectorHit, DetectorId, DetectorOrchestrator, DetectorRegistry, ThresholdValue,
};
use trashradar_app::ports::{HotIndex, ScanEnvironment};
use trashradar_app::scan_control::{
    run_scan_session, CancellableVolumeScanner, ScanController, ScanProgress, VolumeScanOutcome,
};
use trashradar_app::scan_strategy::{choose_scan_strategy, VolumeCapabilities};
use trashradar_app::workers::CancellationToken;
use trashradar_app::{
    mark_confirmed_groups, mvp_predicate_registry, run_duplicate_cascade_with_cache,
    DecisionSelector, KeepPolicy, LiveTotals, MarkedDuplicateGroup, MemoryHashCache,
};
use trashradar_domain::candidate::{
    CandidateId, CandidateUnit, Decision, FileKind, FileRecord, SafetyLevel,
};
use trashradar_domain::category::{CategoryId, CategoryMask};
use trashradar_domain::duplicates::DuplicatesCategoryState;
use trashradar_domain::error::{CoreError, ErrorCode};
use trashradar_domain::scan::ScanStrategy;
use trashradar_domain::settings::AppSettings;
use trashradar_hash::FileHasher;
use trashradar_index_memory::InMemoryIndex;
use trashradar_platform_win::WinScanEnvironment;
use trashradar_scan_walk::{full_path, PathExclusions, WalkConfig};

use crate::events;
use crate::ipc::{record_command, record_command_error};

/// Спільний стан Core для скану (managed Tauri State).
pub struct ScanRuntime {
    pub controller: Arc<ScanController>,
    pub index: Arc<InMemoryIndex>,
    /// Останній live-підсумок (T-055) — для health/діагностики.
    pub last_totals: Arc<Mutex<LiveTotals>>,
    /// Усі категорії кожного кандидата (T-121: «також у: …»), а не лише
    /// primary hit, що живе на `FileRecord.category`. Транзиентно — як і
    /// `last_totals`, перебудовується при кожному скані/перерахунку порогів,
    /// не персистується (той самий принцип, що й `CategoryId::Uncategorized`).
    also_in: Arc<Mutex<HashMap<u64, CategoryMask>>>,
    settings: Arc<RwLock<AppSettings>>,
    /// Останній стан + підтверджені групи каскаду дублікатів (T-058…066,
    /// зібрано в T-126). Транзиентно, як і `also_in` — перебудовується
    /// щоразу на новому скані, не персистується.
    duplicates: Arc<Mutex<DuplicatesRuntimeState>>,
    /// Кеш хешів size+mtime (T-062) — переживає повторні каскади в межах
    /// сесії застосунку; окремий крейт `trashradar-hash` дає сам `Hasher`.
    hash_cache: Arc<MemoryHashCache>,
}

/// Кешований результат останнього каскаду дублікатів для `duplicates.groups` (T-126).
#[derive(Default)]
struct DuplicatesRuntimeState {
    state: DuplicatesCategoryState,
    groups: Vec<MarkedDuplicateGroup>,
}

impl ScanRuntime {
    pub fn new() -> Self {
        Self::with_settings(AppSettings::default())
    }

    pub fn with_settings(settings: AppSettings) -> Self {
        Self {
            controller: Arc::new(ScanController::new()),
            index: Arc::new(InMemoryIndex::new()),
            last_totals: Arc::new(Mutex::new(LiveTotals::new())),
            also_in: Arc::new(Mutex::new(HashMap::new())),
            settings: Arc::new(RwLock::new(settings)),
            duplicates: Arc::new(Mutex::new(DuplicatesRuntimeState::default())),
            hash_cache: Arc::new(MemoryHashCache::new()),
        }
    }

    pub fn apply_settings(&self, settings: &AppSettings) -> Result<u64, CoreError> {
        let registry = configured_registry(settings)?;
        let stats = DetectorOrchestrator::new(&registry)
            .recalculate_index(self.index.as_ref(), &CancellationToken::new())?;
        *self.settings.write().expect("scan settings lock") = settings.clone();
        let records = self.index.get_all();

        // T-121: перерахунок «також у» після зміни порогів — recalculate_index
        // уже прогнав ці ж hits і відкинув усі, крім primary; друга легка
        // вибірка (evaluate_record без запису в індекс) збирає повний набір
        // категорій на кандидата.
        {
            let mut also_in = self.also_in.lock().expect("also_in lock");
            also_in.clear();
            for record in &records {
                if record.decision == Decision::Keep {
                    continue;
                }
                record_hits(&mut also_in, &registry.evaluate_record(record));
            }
        }

        let mut totals = self.last_totals.lock().expect("live totals lock");
        totals.clear();
        totals.ingest_primary(&records);
        Ok(stats.records_seen)
    }

    /// Категорії кандидата за винятком `primary` (маркер «також у: …», T-121).
    pub fn also_in_categories(
        &self,
        candidate_id: CandidateId,
        primary: CategoryId,
    ) -> Vec<CategoryId> {
        let also_in = self.also_in.lock().expect("also_in lock");
        match also_in.get(&candidate_id.0) {
            Some(mask) => mask.iter_excluding(primary).collect(),
            None => Vec::new(),
        }
    }

    /// Останній кешований результат каскаду дублікатів (T-126, `duplicates.groups`).
    /// Порожньо/idle до завершення першого скану, що знайшов ≥2 файли для порівняння.
    pub fn duplicates_snapshot(&self) -> (DuplicatesCategoryState, Vec<MarkedDuplicateGroup>) {
        let dup = self.duplicates.lock().expect("duplicates lock");
        (dup.state.clone(), dup.groups.clone())
    }
}

/// Згорнути hits (T-121) у карту candidateId→CategoryMask.
fn record_hits(also_in: &mut HashMap<u64, CategoryMask>, hits: &[DetectorHit]) {
    for hit in hits {
        also_in
            .entry(hit.candidate_id.0)
            .or_default()
            .insert(hit.verdict.category);
    }
}

/// T-126: каскад пошуку дублікатів (T-058…066) над свіжим індексом по
/// завершенні основного проходу скану.
///
/// Кожна НЕ-keep (╳) позиція групи, що ще не мала своєї категорії
/// (`Uncategorized`), отримує primary `CategoryId::Duplicates` — той самий
/// принцип «перший власник категорії», що й будь-який детектор T-037, лише
/// джерело hit'ів не ферма, а каскад. Учасник з іншою primary-категорією
/// лишається там, отримуючи лише «також у: Дублікати» (T-121-стиль
/// `also_in`). Keep-екземпляр (✓) НІКОЛИ не чіпається — інакше збережена
/// копія рахувалась би в «можна звільнити» (T-054).
fn run_duplicates_cascade<R: Runtime>(
    app: &AppHandle<R>,
    index: &InMemoryIndex,
    last_totals: &Mutex<LiveTotals>,
    also_in: &Mutex<HashMap<u64, CategoryMask>>,
    duplicates: &Mutex<DuplicatesRuntimeState>,
    hash_cache: &MemoryHashCache,
    cancel: &CancellationToken,
) {
    let records: Vec<FileRecord> = index
        .get_all()
        .into_iter()
        .filter(|r| r.unit == CandidateUnit::File && r.decision != Decision::Keep)
        .collect();
    if records.len() < 2 {
        return;
    }

    let hasher = FileHasher::new();
    let result =
        run_duplicate_cascade_with_cache(&records, &hasher, cancel, 0, Some(hash_cache), |state| {
            events::emit_duplicates_cascade(app, state)
        });
    if let Ok(mut dup) = duplicates.lock() {
        dup.state = result.state.clone();
    }
    if result.confirmed_groups.is_empty() {
        return;
    }

    let marked = mark_confirmed_groups(&result.confirmed_groups, &records, KeepPolicy::default());

    let by_id: HashMap<CandidateId, &FileRecord> =
        records.iter().map(|r| (r.candidate_id, r)).collect();
    let mut promoted = Vec::new();
    if let Ok(mut also_in_guard) = also_in.lock() {
        for group in &marked {
            for member in &group.members {
                if member.keep {
                    continue; // збережена копія — не reclaimable, не чіпаємо
                }
                also_in_guard
                    .entry(member.candidate_id.0)
                    .or_default()
                    .insert(CategoryId::Duplicates);
                if let Some(rec) = by_id.get(&member.candidate_id) {
                    if rec.category == CategoryId::Uncategorized {
                        let mut promoted_rec = (*rec).clone();
                        promoted_rec.category = CategoryId::Duplicates;
                        promoted_rec.safety = SafetyLevel::SafeToBulk;
                        promoted_rec.detector_id = "duplicates_cascade".into();
                        promoted_rec.explanation =
                            duplicate_explanation(group.size.0, group.members.len());
                        promoted.push(promoted_rec);
                    }
                }
            }
        }
    }
    if !promoted.is_empty() {
        if let Err(e) = index.upsert_batch(promoted) {
            tracing::warn!(error = %e, "T-126: не вдалося застосувати категорію Дублікати");
        }
    }
    if let Ok(mut live) = last_totals.lock() {
        // Додатково (не reset): нові Duplicates-записи раніше були
        // Uncategorized і не мали внеску в жодну категорію — для решти
        // ingest_primary лише підтверджує вже наявний внесок (no-op).
        live.ingest_primary(&index.get_all());
        events::emit_cleanup_totals(app, &live.summary());
    }
    if let Ok(mut dup) = duplicates.lock() {
        dup.groups = marked;
    }
}

fn duplicate_explanation(group_size_bytes: u64, member_count: usize) -> String {
    let gb = group_size_bytes as f64 / (1024.0 * 1024.0 * 1024.0);
    format!("дублікат · {member_count} копії по {gb:.1} ГБ")
}

fn configured_registry(settings: &AppSettings) -> Result<DetectorRegistry, CoreError> {
    let registry = mvp_predicate_registry();
    for (id, detector) in &settings.detectors {
        let detector_id = match id.as_str() {
            "large_files" => DetectorId::new("large_files"),
            "old_files" => DetectorId::new("old_files"),
            "forgotten_videos" => DetectorId::new("forgotten_videos"),
            "archives" => DetectorId::new("archives"),
            "installers" => DetectorId::new("installers"),
            _ => {
                return Err(CoreError::invalid_argument(format!(
                    "Unknown detector: {id}"
                )))
            }
        };
        for (key, value) in &detector.thresholds {
            registry.set_threshold(detector_id, key, ThresholdValue::U64(*value))?;
        }
    }
    Ok(registry)
}

impl Default for ScanRuntime {
    fn default() -> Self {
        Self::new()
    }
}

/// Параметри `scan.start`.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ScanStartPayload {
    /// Літери томів (`"C"`, `"D:"`…). Порожньо / omitted = усі з середовища.
    #[serde(default)]
    pub volumes: Option<Vec<String>>,
}

/// Ack `scan.start` (неблокуюче прийняття).
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ScanStartAck {
    pub accepted: bool,
    pub volume_count: u32,
}

/// Ack `scan.stop`.
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ScanStopAck {
    /// Чи був активний скан, якому надіслано cancel.
    pub stopping: bool,
}

/// Payload `scan.progress`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ScanProgressEvent {
    pub volume: String,
    pub strategy: String,
    pub phase: String,
    pub files_indexed: u64,
    pub volume_index: u32,
    pub volume_count: u32,
    pub done: bool,
    pub cancelled: bool,
}

impl ScanProgressEvent {
    fn from_progress(p: &ScanProgress) -> Self {
        Self {
            volume: format!("{}:", p.volume.to_ascii_uppercase()),
            strategy: p.strategy.as_str().to_string(),
            phase: p.phase.as_str().to_string(),
            files_indexed: p.files_indexed,
            volume_index: p.volume_index,
            volume_count: p.volume_count,
            done: p.done,
            cancelled: p.cancelled,
        }
    }
}

fn parse_volume_letter(s: &str) -> Option<char> {
    let c = s.trim().chars().next()?;
    if c.is_ascii_alphabetic() {
        Some(c.to_ascii_uppercase())
    } else {
        None
    }
}

fn resolve_volumes(payload: &ScanStartPayload) -> Result<Vec<char>, CoreError> {
    if let Some(list) = &payload.volumes {
        if list.is_empty() {
            return Err(CoreError::invalid_argument(
                "Список томів порожній. Передайте літери або не вказуйте volumes.",
            ));
        }
        let mut out = Vec::new();
        for s in list {
            let Some(v) = parse_volume_letter(s) else {
                return Err(CoreError::invalid_argument(format!(
                    "Некоректна літера тому «{s}»."
                )));
            };
            if !out.contains(&v) {
                out.push(v);
            }
        }
        return Ok(out);
    }
    let env = WinScanEnvironment;
    let vols = env.list_scan_volumes();
    if vols.is_empty() {
        return Err(CoreError::new(
            ErrorCode::Io,
            "Не знайдено томів для сканування.".to_string(),
        ));
    }
    Ok(vols)
}

fn strategies_for(volumes: &[char]) -> Vec<ScanStrategy> {
    let env = WinScanEnvironment;
    let elevated = env.is_elevated();
    volumes
        .iter()
        .map(|&v| {
            let is_ntfs = env.is_ntfs(v).unwrap_or(false);
            choose_scan_strategy(&VolumeCapabilities {
                is_ntfs,
                is_elevated: elevated,
            })
            .strategy
        })
        .collect()
}

/// Диспетчер: MFT або walk за T-028 на кожному томі.
struct AutoVolumeScanner;

impl CancellableVolumeScanner for AutoVolumeScanner {
    fn scan_volume(
        &self,
        volume: char,
        cancel: &CancellationToken,
        on_batch: &mut dyn FnMut(Vec<FileRecord>) -> Result<(), CoreError>,
    ) -> Result<VolumeScanOutcome, CoreError> {
        scan_one_volume_auto(volume, cancel, on_batch)
    }
}

fn scan_one_volume_auto(
    volume: char,
    cancel: &CancellationToken,
    on_batch: &mut dyn FnMut(Vec<FileRecord>) -> Result<(), CoreError>,
) -> Result<VolumeScanOutcome, CoreError> {
    let env = WinScanEnvironment;
    let strategy = choose_scan_strategy(&VolumeCapabilities {
        is_ntfs: env.is_ntfs(volume).unwrap_or(false),
        is_elevated: env.is_elevated(),
    })
    .strategy;
    match strategy {
        ScanStrategy::Mft => scan_mft_cancellable(volume, cancel, on_batch),
        ScanStrategy::DirectoryWalk | ScanStrategy::UsnDelta => {
            scan_walk_cancellable(volume, cancel, on_batch)
        }
    }
}

fn scan_mft_cancellable(
    volume: char,
    cancel: &CancellationToken,
    on_batch: &mut dyn FnMut(Vec<FileRecord>) -> Result<(), CoreError>,
) -> Result<VolumeScanOutcome, CoreError> {
    #[cfg(windows)]
    {
        const BATCH: usize = 10_000;
        let (stats, cancelled) = trashradar_scan_mft::pipeline::scan_volume_to_index_cancel(
            volume,
            BATCH,
            || cancel.is_cancelled(),
            on_batch,
        )?;
        Ok(VolumeScanOutcome {
            files_indexed: stats.files_indexed,
            cancelled,
        })
    }
    #[cfg(not(windows))]
    {
        let _ = (volume, cancel, on_batch);
        Err(CoreError::not_implemented("scan.mft"))
    }
}

fn scan_walk_cancellable(
    volume: char,
    cancel: &CancellationToken,
    on_batch: &mut dyn FnMut(Vec<FileRecord>) -> Result<(), CoreError>,
) -> Result<VolumeScanOutcome, CoreError> {
    if cancel.is_cancelled() {
        return Ok(VolumeScanOutcome {
            files_indexed: 0,
            cancelled: true,
        });
    }

    let config = WalkConfig {
        exclusions: PathExclusions::windows_system_defaults(volume),
        ..WalkConfig::default()
    };
    let all = trashradar_scan_walk::walk_volume(volume, config)?;
    if cancel.is_cancelled() {
        return Ok(VolumeScanOutcome {
            files_indexed: 0,
            cancelled: true,
        });
    }

    const BATCH: usize = 5_000;
    let mut files_indexed = 0u64;
    let mut batch = Vec::new();
    let mut next_id = 0u64;

    for e in &all {
        if cancel.is_cancelled() {
            if !batch.is_empty() {
                files_indexed += batch.len() as u64;
                on_batch(std::mem::take(&mut batch))?;
            }
            return Ok(VolumeScanOutcome {
                files_indexed,
                cancelled: true,
            });
        }
        if e.is_directory {
            continue;
        }
        let Some(path) = full_path(volume, &all, e) else {
            continue;
        };
        batch.push(FileRecord {
            candidate_id: CandidateId(next_id),
            path: path.clone(),
            size: e.size,
            created_at: e.created_at,
            modified_at: e.modified_at,
            accessed_at: e.accessed_at,
            kind: FileKind::from_path(&path),
            unit: CandidateUnit::File,
            category: CategoryId::Uncategorized,
            safety: SafetyLevel::ReviewRecommended,
            decision: Decision::Undecided,
            detector_id: String::new(),
            explanation: String::new(),
            attributes: e.attributes,
        });
        next_id += 1;
        if batch.len() >= BATCH {
            files_indexed += batch.len() as u64;
            on_batch(std::mem::take(&mut batch))?;
        }
    }
    if !batch.is_empty() {
        files_indexed += batch.len() as u64;
        on_batch(batch)?;
    }
    Ok(VolumeScanOutcome {
        files_indexed,
        cancelled: false,
    })
}

fn emit_progress<R: Runtime>(app: &AppHandle<R>, p: &ScanProgress) {
    events::emit(
        app,
        events::topic::SCAN_PROGRESS,
        &ScanProgressEvent::from_progress(p),
    );
}

/// `scan.start` — ack одразу, робота у фоні.
#[tauri::command]
pub async fn scan_start<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, ScanRuntime>,
    payload: Option<ScanStartPayload>,
) -> Result<ScanStartAck, CoreError> {
    record_command();
    let payload = payload.unwrap_or_default();
    let volumes = match resolve_volumes(&payload) {
        Ok(v) => v,
        Err(e) => {
            record_command_error();
            return Err(e);
        }
    };
    let strategies = strategies_for(&volumes);
    let volume_count = volumes.len() as u32;

    let token = match state.controller.begin() {
        Ok(t) => t,
        Err(e) => {
            record_command_error();
            return Err(e);
        }
    };

    let controller = Arc::clone(&state.controller);
    let index = Arc::clone(&state.index);
    let last_totals = Arc::clone(&state.last_totals);
    let also_in = Arc::clone(&state.also_in);
    let settings = Arc::clone(&state.settings);
    let duplicates = Arc::clone(&state.duplicates);
    let hash_cache = Arc::clone(&state.hash_cache);
    let app2 = app.clone();

    if let Err(e) = thread::Builder::new()
        .name("trashradar-scan".into())
        .spawn(move || {
            // Свіжий live-стан на старті сесії (T-055 / T-121).
            if let Ok(mut live) = last_totals.lock() {
                live.clear();
            }
            if let Ok(mut also_in) = also_in.lock() {
                also_in.clear();
            }
            let settings = settings.read().expect("scan settings lock").clone();
            let farm = match configured_registry(&settings) {
                Ok(farm) => farm,
                Err(error) => {
                    record_command_error();
                    eprintln!("settings apply failed: {error}");
                    return;
                }
            };
            let orch = DetectorOrchestrator::new(&farm);
            let mut throttle = events::AggregateThrottle::new_at(
                Duration::from_millis(100), // ≤10/с (T-006)
                Instant::now(),
            );

            let scanner = AutoVolumeScanner;
            let result = run_scan_session(
                &volumes,
                &strategies,
                &scanner,
                &token,
                |p| emit_progress(&app2, &p),
                |_vol, batch| {
                    // 1) сирі записи в індекс
                    index.insert_batch(batch.clone())?;
                    // 2) детектори → primary + hits
                    let cat = orch.categorize_batch(&batch);
                    if !cat.updated.is_empty() {
                        index.upsert_batch(cat.updated)?;
                    }
                    // 3) «також у» з тих самих hits (T-121) — жодного зайвого
                    // прогону детекторів, лише запам'ятати повний набір категорій.
                    if let Ok(mut also_in) = also_in.lock() {
                        record_hits(&mut also_in, &cat.hits);
                    }
                    // 4) live totals + throttled UI events (≤10/с)
                    if let Ok(mut live) = last_totals.lock() {
                        live.ingest_hits(&cat.hits, &batch);
                        let summary = live.summary();
                        if let Some(snap) = throttle.observe(Instant::now(), summary) {
                            events::emit_cleanup_totals(&app2, &snap);
                        }
                    }
                    Ok(())
                },
            );
            // T-126: каскад пошуку дублікатів (T-058…066) над свіжим індексом —
            // той самий `token`, тому `scan.stop` скасовує і хешування.
            run_duplicates_cascade(
                &app2,
                index.as_ref(),
                &last_totals,
                &also_in,
                &duplicates,
                &hash_cache,
                &token,
            );

            // Фінальний snapshot без втрати підсумку (T-055 / T-006 flush).
            let _ = throttle.flush(Instant::now());
            if let Ok(live) = last_totals.lock() {
                let summary = live.summary();
                events::emit_cleanup_totals(&app2, &summary);
                // InMemoryIndex::get_all — inherent Vec; trait HotIndex returns Result.
                let all = index.get_all();
                let matches = live.unique_matches_index(&all);
                tracing::info!(
                    reclaimable = summary.unique_bytes.0,
                    unique_files = summary.unique_files,
                    matches_index = matches,
                    "live totals після скану"
                );
                debug_assert!(matches, "T-055: live unique має збігатися з індексом");
            }
            match &result {
                Ok(summary) => tracing::info!(
                    files = summary.files_indexed,
                    volumes = summary.volumes_completed,
                    cancelled = summary.cancelled,
                    "сесія скану завершена"
                ),
                Err(e) => tracing::error!(error = %e, "сесія скану завершилась з помилкою"),
            }
            controller.end();
        })
    {
        state.controller.end();
        record_command_error();
        return Err(CoreError::internal(format!(
            "Не вдалося запустити скан: {e}"
        )));
    }

    tracing::debug!(volume_count, "scan.start accepted");
    Ok(ScanStartAck {
        accepted: true,
        volume_count,
    })
}

/// `scan.stop` — кооперативна відміна (DoD ≤ 500 мс на боці сканера).
#[tauri::command]
pub async fn scan_stop(state: State<'_, ScanRuntime>) -> Result<ScanStopAck, CoreError> {
    record_command();
    let stopping = state.controller.request_cancel();
    tracing::debug!(stopping, "scan.stop");
    Ok(ScanStopAck { stopping })
}

// --- T-057: candidate.keep / candidate.mark ----------------------------------

/// Payload `candidate.keep` / `candidate.mark`.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CandidateDecisionPayload {
    pub candidate_id: Option<u64>,
    pub path: Option<String>,
    /// Лише для mark: `false` = unmark → Undecided.
    #[serde(default)]
    pub marked: Option<bool>,
    /// `candidate.keep` з `unkeep: true` → знову Undecided.
    #[serde(default)]
    pub unkeep: Option<bool>,
}

/// Ack keep/mark.
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CandidateDecisionAck {
    pub updated: u32,
    pub decision: String,
}

fn selector_from_payload(
    payload: &CandidateDecisionPayload,
) -> Result<DecisionSelector, CoreError> {
    let mut sel = DecisionSelector::default();
    if let Some(id) = payload.candidate_id {
        sel.candidate_ids.push(CandidateId(id));
    }
    if let Some(path) = &payload.path {
        if !path.trim().is_empty() {
            sel.paths.push(path.clone());
        }
    }
    if sel.is_empty() {
        return Err(CoreError::invalid_argument("Вкажіть candidateId або path."));
    }
    Ok(sel)
}

fn decision_wire(d: Decision) -> &'static str {
    match d {
        Decision::Undecided => "undecided",
        Decision::Keep => "keep",
        Decision::Marked => "marked",
    }
}

fn apply_and_emit_totals<R: Runtime>(
    app: &AppHandle<R>,
    state: &ScanRuntime,
    result: &trashradar_app::ApplyDecisionResult,
) {
    if let Ok(mut live) = state.last_totals.lock() {
        for r in &result.updated {
            live.set_decision(r.candidate_id, result.decision);
        }
        events::emit_cleanup_totals(app, &live.summary());
    }
}

/// `candidate.keep` — Keep (або unkeep) на файлі → усі категорії (T-057).
#[tauri::command]
pub async fn candidate_keep<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, ScanRuntime>,
    payload: CandidateDecisionPayload,
) -> Result<CandidateDecisionAck, CoreError> {
    record_command();
    let selector = match selector_from_payload(&payload) {
        Ok(s) => s,
        Err(e) => {
            record_command_error();
            return Err(e);
        }
    };
    let decision = if payload.unkeep == Some(true) {
        Decision::Undecided
    } else {
        Decision::Keep
    };
    let result = match trashradar_app::apply_decision_hot(state.index.as_ref(), &selector, decision)
    {
        Ok(r) => r,
        Err(e) => {
            record_command_error();
            return Err(e);
        }
    };
    apply_and_emit_totals(&app, &state, &result);
    tracing::debug!(
        updated = result.count(),
        decision = decision_wire(decision),
        "candidate.keep"
    );
    Ok(CandidateDecisionAck {
        updated: result.count() as u32,
        decision: decision_wire(decision).into(),
    })
}

/// `candidate.mark` — Marked / Undecided на файлі (T-057).
#[tauri::command]
pub async fn candidate_mark<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, ScanRuntime>,
    payload: CandidateDecisionPayload,
) -> Result<CandidateDecisionAck, CoreError> {
    record_command();
    let selector = match selector_from_payload(&payload) {
        Ok(s) => s,
        Err(e) => {
            record_command_error();
            return Err(e);
        }
    };
    let marked = payload.marked.unwrap_or(true);
    let result = match trashradar_app::mark_hot(state.index.as_ref(), &selector, marked) {
        Ok(r) => r,
        Err(e) => {
            record_command_error();
            return Err(e);
        }
    };
    apply_and_emit_totals(&app, &state, &result);
    tracing::debug!(
        updated = result.count(),
        decision = decision_wire(result.decision),
        "candidate.mark"
    );
    Ok(CandidateDecisionAck {
        updated: result.count() as u32,
        decision: decision_wire(result.decision).into(),
    })
}

/// Параметри `candidate.reveal_in_explorer`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RevealInExplorerPayload {
    pub candidate_id: u64,
}

/// «Показати у провіднику» (T-125, docs/ui.md §6): відкрити Explorer із
/// виділеним файлом кандидата. Той самий лукап за candidate_id, що й
/// `preview.thumbnail`/`preview.large` (T-120/T-124) — жодного дублювання.
#[tauri::command]
pub fn candidate_reveal_in_explorer(
    payload: RevealInExplorerPayload,
    scan: State<'_, ScanRuntime>,
) -> Result<(), CoreError> {
    record_command();
    let record = match crate::preview_runtime::find_record(&scan, payload.candidate_id) {
        Ok(r) => r,
        Err(e) => {
            record_command_error();
            return Err(e);
        }
    };
    if let Err(e) = trashradar_platform_win::reveal_in_explorer(&record.path) {
        record_command_error();
        return Err(e);
    }
    Ok(())
}

// --- T-126: duplicates.groups -------------------------------------------------

/// Дзеркало домену `MarkedDuplicateGroup` для UI (`ui/src/ipc/types.ts`):
/// лише `contentHash` відрізняється — `ContentHash([u8;32])` серіалізувався б
/// як JSON-масив 32 чисел, тут — hex-рядок. Решта полів (`MarkedDuplicateMember`,
/// `KeepPolicy`) уже серіалізуються як треба (camelCase/snake_case), тому
/// членів групи переносимо без перетворення.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MarkedDuplicateGroupDto {
    pub size: u64,
    pub content_hash: String,
    pub members: Vec<trashradar_domain::duplicates::MarkedDuplicateMember>,
    pub keep_id: u64,
    pub policy: KeepPolicy,
    pub potential_reclaim_bytes: u64,
}

impl From<&MarkedDuplicateGroup> for MarkedDuplicateGroupDto {
    fn from(g: &MarkedDuplicateGroup) -> Self {
        Self {
            size: g.size.0,
            content_hash: g.content_hash.to_hex(),
            members: g.members.clone(),
            keep_id: g.keep_id.0,
            policy: g.policy,
            potential_reclaim_bytes: g.potential_reclaim_bytes,
        }
    }
}

/// Відповідь `duplicates.groups` (T-126): останній кешований результат
/// каскаду T-058…066 + дефолтна розмітка ✓/╳ (T-065). `state` — той самий
/// знімок, що й подія `duplicates.cascade_updated` (T-061), для екрана, який
/// відкрився вже після завершення каскаду (подія проґавлена).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DuplicatesGroupsAck {
    pub state: events::DuplicatesCascadeEvent,
    pub groups: Vec<MarkedDuplicateGroupDto>,
}

/// `duplicates.groups` — групи для «Дублікати» (T-126, docs/ui.md §4):
/// сітка групами замість стандартної `category.window`. Порожньо/idle до
/// першого завершеного скану, що знайшов ≥2 файли для порівняння.
#[tauri::command]
pub fn duplicates_groups(scan: State<'_, ScanRuntime>) -> Result<DuplicatesGroupsAck, CoreError> {
    record_command();
    let (state, groups) = scan.duplicates_snapshot();
    Ok(DuplicatesGroupsAck {
        state: events::DuplicatesCascadeEvent::from_state(&state),
        groups: groups.iter().map(MarkedDuplicateGroupDto::from).collect(),
    })
}

// --- T-138: reap.execute / reap.undo_batch --------------------------------

fn volume_of(path: &str) -> Option<char> {
    let bytes = path.as_bytes();
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        Some(bytes[0].to_ascii_uppercase() as char)
    } else {
        None
    }
}

/// Параметри `reap.execute` (T-138): candidateId уже позначених/підтверджених
/// в оверлеї (T-135…T-137) — той самий id-простір, що й `candidate.batch`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReapExecutePayload {
    pub candidate_ids: Vec<u64>,
}

/// Відповідь `reap.execute`: підсумок для тосту «X ГБ у Quarantine · [Скасувати]».
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReapExecuteAck {
    pub batch_id: u64,
    pub reaped_count: u64,
    pub reaped_bytes: u64,
}

/// `reap.execute` (T-138): транзакційний батч-reap уже позначених candidateId
/// (кнопка «Відправити у Quarantine», ReapConfirmOverlay T-135…T-137). Use
/// case (`TransactionalReaper`, T-079) уже вміє все — durable in_flight →
/// atomic move → durable quarantined + аудит; тут лише резолв сурогатного
/// шляху/тому призначення (той самий примітив, що й `quarantine.thumbnail`/
/// `restore_batch`, T-130/T-132) і побудова `QuarantineEntry` на кожен файл.
///
/// Зреапнуті записи приховуються з усіх категорій сесії тим самим шляхом,
/// що й Keep (T-057, `apply_decision_hot` + `apply_and_emit_totals`):
/// `Decision` не має власного варіанта «вже переміщено в Quarantine» —
/// додавання нового варіанта torкнулось би усіх match-виразів по Decision
/// у domain/app/shell/UI заради одного разового приховання в межах сесії;
/// Keep уже означає саме «не показувати більше в жодній категорії», що тут
/// і потрібно (сам файл фізично в Quarantine, не «залишений» користувачем,
/// але для UI-видимості це той самий ефект).
#[tauri::command]
pub async fn reap_execute<R: Runtime>(
    app: AppHandle<R>,
    payload: ReapExecutePayload,
    state: State<'_, ScanRuntime>,
    profile: State<'_, crate::ipc::ProfileRuntime>,
    settings: State<'_, crate::ipc::SettingsRuntime>,
) -> Result<ReapExecuteAck, CoreError> {
    record_command();
    if payload.candidate_ids.is_empty() {
        record_command_error();
        return Err(CoreError::invalid_argument("Список candidateIds порожній."));
    }

    let wanted: std::collections::HashSet<u64> = payload.candidate_ids.iter().copied().collect();
    let records: Vec<FileRecord> = state
        .index
        .get_all()
        .into_iter()
        .filter(|r| wanted.contains(&r.candidate_id.0))
        .collect();
    if records.is_empty() {
        record_command_error();
        return Err(CoreError::invalid_argument(
            "Жоден з candidateIds не знайдено в живому індексі.",
        ));
    }

    let profile_dir = profile.profile_dir();
    let ttl_days = settings.current().quarantine.ttl_days;
    let batch_id = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);

    let records_for_reap = records.clone();
    let outcomes = tauri::async_runtime::spawn_blocking(move || {
        execute_reap_batch(profile_dir, records_for_reap, ttl_days, batch_id)
    })
    .await
    .map_err(|error| CoreError::internal(format!("Reap task failed: {error}")))?;

    let outcomes = match outcomes {
        Ok(outcomes) => outcomes,
        Err(error) => {
            record_command_error();
            return Err(error);
        }
    };

    let selector = DecisionSelector {
        candidate_ids: records.iter().map(|r| r.candidate_id).collect(),
        paths: Vec::new(),
    };
    if let Ok(result) =
        trashradar_app::apply_decision_hot(state.index.as_ref(), &selector, Decision::Keep)
    {
        apply_and_emit_totals(&app, &state, &result);
    }

    let reaped_bytes: u64 = outcomes.iter().map(|o| o.entry.size.0).sum();
    Ok(ReapExecuteAck {
        batch_id,
        reaped_count: outcomes.len() as u64,
        reaped_bytes,
    })
}

fn execute_reap_batch(
    profile: Option<std::path::PathBuf>,
    records: Vec<FileRecord>,
    ttl_days: u32,
    batch_id: u64,
) -> Result<Vec<trashradar_app::ReapOutcome>, CoreError> {
    use trashradar_domain::quarantine::{
        BatchId, QuarantineEntry, QuarantineEntryId, QuarantineStatus,
    };

    let profile =
        profile.ok_or_else(|| CoreError::invalid_argument("Профіль Quarantine недоступний."))?;
    let profile_root = profile.to_string_lossy().into_owned();
    let database = trashradar_index_sqlite::IndexDatabase::open_profile(profile)
        .map_err(|error| CoreError::internal(format!("Manifest не відкрився: {error}")))?;
    let filesystem = trashradar_quarantine_fs::NativeQuarantineFs;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let expires_at = now.saturating_add(i64::from(ttl_days) * 86_400);

    let mut requests = Vec::with_capacity(records.len());
    for record in &records {
        // UNC/нелокальний шлях — не резолвиться на том, пропускаємо (той
        // самий fail-open принцип, що й candidate.batch: невідома позиція
        // не валить весь батч).
        let Some(volume) = volume_of(&record.path) else {
            continue;
        };

        // Гарячий індекс (T-024/T-057, infra/index-memory::CompactFileRecord)
        // зберігає modified_at лише з точністю до цілої секунди (u32 unix
        // secs) заради економії пам'яті на весь сеанс сканування — тоді як
        // жива перевірка нижче (T-086, TransactionalReaper::reap_one) звіряє
        // повну 100-нс FILETIME-точність. Порівняння впритул record.modified_at
        // (завжди округлене) з живим точним читанням майже завжди хибно
        // відмовляло б із file_changed для БУДЬ-ЯКОГО реального файла. Тому
        // тут звіряємо на найкращій точності, яку справді захопило сканування
        // (розмір точно, mtime до цілої секунди), а якщо збігається —
        // передаємо в TransactionalReaper щойно прочитане ТОЧНЕ значення як
        // expected_identity: внутрішня сувора перевірка тоді звіряє себе з
        // тим самим моментом і коректно ловить лише зміни в короткому вікні
        // між цим читанням і фактичним move.
        let live = trashradar_platform_win::read_file_identity(std::path::Path::new(&record.path))?;
        let record_seconds = record.modified_at.map(|timestamp| timestamp.0 / 10_000_000);
        let live_seconds = live.modified_at.map(|timestamp| timestamp.0 / 10_000_000);
        if live.size != record.size || live_seconds != record_seconds {
            return Err(CoreError::file_changed(format!(
                "Файл «{}» змінився після сканування; reap скасовано.",
                record.path
            )));
        }

        let directory = filesystem.ensure_on_volume(volume)?;
        let surrogate_name = format!("{:016}.bin", record.candidate_id.0);
        let destination = directory.quarantine_root.join(&surrogate_name);
        requests.push(trashradar_app::ReapRequest {
            entry: QuarantineEntry {
                id: QuarantineEntryId(record.candidate_id.0),
                batch_id: Some(BatchId(batch_id)),
                original_path: record.path.clone(),
                surrogate_name,
                size: record.size,
                quarantined_at_unix: now,
                expires_at_unix: expires_at,
                status: QuarantineStatus::InFlight,
            },
            destination_path: destination.to_string_lossy().into_owned(),
            expected_identity: live,
        });
    }
    if requests.is_empty() {
        return Err(CoreError::invalid_argument(
            "Жоден позначений файл не резолвився на локальний том.",
        ));
    }

    let roots = vec![profile_root];
    trashradar_app::TransactionalReaper::new(&filesystem, &database, &roots)
        .reap_batch(BatchId(batch_id), requests)
}

/// Параметри `reap.undo_batch` (T-138): «Скасувати» на тості після reap.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReapUndoBatchPayload {
    pub batch_id: u64,
}

/// `reap.undo_batch` (T-138, use case T-081): відновити весь батч одним
/// викликом. Manifest сам знаходить усі quarantined-записи батчу
/// (`undo_batch`); тут лише резолв сурогатного шляху (той самий примітив,
/// що й `quarantine.thumbnail`/`restore_batch`) і подія-міст на кожен запис.
#[tauri::command]
pub async fn reap_undo_batch<R: Runtime>(
    app: AppHandle<R>,
    payload: ReapUndoBatchPayload,
    profile: State<'_, crate::ipc::ProfileRuntime>,
) -> Result<Vec<crate::ipc::QuarantineRestoredDto>, CoreError> {
    record_command();
    let profile_dir = profile.profile_dir();
    let batch_id = payload.batch_id;
    let outcomes =
        tauri::async_runtime::spawn_blocking(move || undo_reap_batch(profile_dir, batch_id))
            .await
            .map_err(|error| CoreError::internal(format!("Reap undo task failed: {error}")))?;
    let outcomes = match outcomes {
        Ok(outcomes) => outcomes,
        Err(error) => {
            record_command_error();
            return Err(error);
        }
    };

    let mut dtos = Vec::with_capacity(outcomes.len());
    for outcome in &outcomes {
        events::emit_quarantine_restored(&app, outcome);
        dtos.push(crate::ipc::QuarantineRestoredDto {
            entry_id: outcome.entry.id.0,
            original_path: outcome.entry.original_path.clone(),
            restored_path: outcome.restored_path.clone(),
            used_suffix: outcome.used_suffix,
        });
    }
    Ok(dtos)
}

fn undo_reap_batch(
    profile: Option<std::path::PathBuf>,
    batch_id: u64,
) -> Result<Vec<trashradar_app::RestoreOutcome>, CoreError> {
    use trashradar_domain::quarantine::BatchId;

    let profile =
        profile.ok_or_else(|| CoreError::invalid_argument("Профіль Quarantine недоступний."))?;
    let database = trashradar_index_sqlite::IndexDatabase::open_profile(profile)
        .map_err(|error| CoreError::internal(format!("Manifest не відкрився: {error}")))?;
    let filesystem = trashradar_quarantine_fs::NativeQuarantineFs;
    trashradar_app::QuarantineRestorer::new(&filesystem, &database).undo_batch(
        BatchId(batch_id),
        |entry| {
            filesystem
                .surrogate_path(&entry.original_path, &entry.surrogate_name)
                .map(|p| p.to_string_lossy().into_owned())
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::io::Write;
    use trashradar_app::scan_control::SteppedTestScanner;
    use trashradar_domain::candidate::{ByteSize, FileAttributes, FsTimestamp};
    use trashradar_domain::duplicates::DuplicateConfidence;
    use trashradar_domain::scan::ScanStrategy;
    use trashradar_domain::settings::DetectorSettings;

    /// Мінімальний mock-застосунок лише для `AppHandle` (T-126: `events::emit`
    /// потребує рантайм, але не потребує вікна — той самий примітив, що й
    /// важчий `test_app()` в `ipc::tests`, без webview/invoke_handler).
    fn mock_app() -> tauri::App<tauri::test::MockRuntime> {
        tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .expect("mock app")
    }

    #[test]
    fn parse_volume_accepts_drive_forms() {
        assert_eq!(parse_volume_letter("C"), Some('C'));
        assert_eq!(parse_volume_letter("d:"), Some('D'));
        assert_eq!(parse_volume_letter("  e:\\"), Some('E'));
        assert_eq!(parse_volume_letter("1"), None);
    }

    #[test]
    fn resolve_explicit_volumes() {
        let p = ScanStartPayload {
            volumes: Some(vec!["C".into(), "D:".into(), "C".into()]),
        };
        let v = resolve_volumes(&p).unwrap();
        assert_eq!(v, vec!['C', 'D']);
    }

    #[test]
    fn detector_threshold_applies_to_existing_index_without_rescan() {
        let runtime = ScanRuntime::new();
        runtime
            .index
            .insert_batch(vec![FileRecord {
                candidate_id: CandidateId(1),
                path: "C:\\medium.bin".into(),
                size: ByteSize(50 * 1024 * 1024),
                created_at: None,
                modified_at: None,
                accessed_at: None,
                kind: FileKind::Other,
                unit: CandidateUnit::File,
                category: CategoryId::Uncategorized,
                safety: SafetyLevel::ReviewRecommended,
                decision: Decision::Undecided,
                detector_id: String::new(),
                explanation: String::new(),
                attributes: FileAttributes::default(),
            }])
            .unwrap();
        let mut settings = AppSettings::default();
        settings.detectors.insert(
            "large_files".into(),
            DetectorSettings {
                thresholds: BTreeMap::from([("min_size_bytes".into(), 10 * 1024 * 1024)]),
            },
        );
        assert_eq!(runtime.apply_settings(&settings).unwrap(), 1);
        let records = runtime.index.get_all();
        assert_eq!(records[0].category, CategoryId::LargeFiles);
    }

    #[test]
    fn ipc_session_cancel_keeps_partial_index() {
        // Ізольований прогін без Tauri: контролер + stepped scanner + hot index.
        let runtime = ScanRuntime::new();
        let token = runtime.controller.begin().unwrap();
        let index = Arc::clone(&runtime.index);
        let scanner = SteppedTestScanner {
            steps: 15,
            step_delay: std::time::Duration::from_millis(40),
            files_per_step: 5,
        };
        let token2 = token.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(80));
            token2.cancel();
        });
        let summary = run_scan_session(
            &['C'],
            &[ScanStrategy::DirectoryWalk],
            &scanner,
            &token,
            |_| {},
            |_, batch| index.insert_batch(batch),
        )
        .unwrap();
        runtime.controller.end();
        assert!(summary.cancelled);
        let n = HotIndex::len(runtime.index.as_ref()).unwrap();
        assert!(n > 0);
        assert_eq!(n as u64, summary.files_indexed);
    }

    /// DoD T-126: каскад дублікатів над реальними файлами (real FileHasher I/O)
    /// підтверджує групу, дефолтна ✓/╳-розмітка приходить із Core, не-keep
    /// (╳) учасник, що був `Uncategorized`, промотується у primary
    /// `CategoryId::Duplicates`; keep-екземпляр (✓) лишається недоторканим і
    /// не рахується в «можна звільнити».
    #[test]
    fn duplicates_cascade_confirms_group_and_promotes_reap_member_only() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("trashradar-dup-cascade-{nonce}"));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path_a = dir.join("a.bin");
        let path_b = dir.join("b.bin");
        let content = vec![7u8; 200_000]; // > 64 КіБ — partial і full обидва читають реальні байти
        std::fs::File::create(&path_a)
            .unwrap()
            .write_all(&content)
            .unwrap();
        std::fs::File::create(&path_b)
            .unwrap()
            .write_all(&content)
            .unwrap();

        fn rec(id: u64, path: &std::path::Path, size: u64, mtime: i64) -> FileRecord {
            FileRecord {
                candidate_id: CandidateId(id),
                path: path.to_string_lossy().into_owned(),
                size: ByteSize(size),
                created_at: None,
                modified_at: Some(FsTimestamp(mtime)),
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

        let runtime = ScanRuntime::new();
        runtime
            .index
            .insert_batch(vec![
                rec(1, &path_a, content.len() as u64, 100),
                rec(2, &path_b, content.len() as u64, 200),
            ])
            .unwrap();

        let app = mock_app();
        run_duplicates_cascade(
            app.handle(),
            runtime.index.as_ref(),
            &runtime.last_totals,
            &runtime.also_in,
            &runtime.duplicates,
            &runtime.hash_cache,
            &CancellationToken::new(),
        );

        let (state, groups) = runtime.duplicates_snapshot();
        assert_eq!(state.confidence, DuplicateConfidence::Confirmed);
        assert_eq!(groups.len(), 1, "рівно одна група дублікатів");
        assert_eq!(groups[0].members.len(), 2);
        assert_eq!(groups[0].members.iter().filter(|m| m.keep).count(), 1);

        let records = runtime.index.get_all();
        let reap_id = groups[0]
            .members
            .iter()
            .find(|m| !m.keep)
            .expect("reap member")
            .candidate_id;
        let keep_id = groups[0].keep_id;
        let reap_record = records.iter().find(|r| r.candidate_id == reap_id).unwrap();
        let keep_record = records.iter().find(|r| r.candidate_id == keep_id).unwrap();
        assert_eq!(
            reap_record.category,
            CategoryId::Duplicates,
            "не-keep учасник, що був Uncategorized, промотується"
        );
        assert_eq!(
            keep_record.category,
            CategoryId::Uncategorized,
            "keep-екземпляр не чіпається"
        );

        let summary = runtime.last_totals.lock().unwrap().summary();
        assert_eq!(summary.unique_files, 1, "лише не-keep копія reclaimable");
        assert_eq!(summary.unique_bytes.0, content.len() as u64);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
