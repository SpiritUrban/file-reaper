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
use trashradar_app::{mvp_predicate_registry, DecisionSelector, LiveTotals};
use trashradar_domain::candidate::{
    CandidateId, CandidateUnit, Decision, FileKind, FileRecord, SafetyLevel,
};
use trashradar_domain::category::{CategoryId, CategoryMask};
use trashradar_domain::error::{CoreError, ErrorCode};
use trashradar_domain::scan::ScanStrategy;
use trashradar_domain::settings::AppSettings;
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use trashradar_app::scan_control::SteppedTestScanner;
    use trashradar_domain::candidate::{ByteSize, FileAttributes};
    use trashradar_domain::scan::ScanStrategy;
    use trashradar_domain::settings::DetectorSettings;

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
}
