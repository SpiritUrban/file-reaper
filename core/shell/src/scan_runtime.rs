//! Рантайм сесії скану в shell (T-033): IPC start/stop + прогрес-події.
//!
//! Бізнес-логіка сесії — `trashradar_app::scan_control`; тут лише
//! Tauri State, фоновий потік і адаптери MFT/walk.

use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Runtime, State};
use trashradar_app::detectors::DetectorOrchestrator;
use trashradar_app::ports::{HotIndex, ScanEnvironment};
use trashradar_app::scan_control::{
    run_scan_session, CancellableVolumeScanner, ScanController, ScanProgress, VolumeScanOutcome,
};
use trashradar_app::scan_strategy::{choose_scan_strategy, VolumeCapabilities};
use trashradar_app::workers::CancellationToken;
use trashradar_app::{mvp_predicate_registry, LiveTotals};
use trashradar_domain::candidate::{
    CandidateId, CandidateUnit, Decision, FileKind, FileRecord, SafetyLevel,
};
use trashradar_domain::category::CategoryId;
use trashradar_domain::error::{CoreError, ErrorCode};
use trashradar_domain::scan::ScanStrategy;
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
}

impl ScanRuntime {
    pub fn new() -> Self {
        Self {
            controller: Arc::new(ScanController::new()),
            index: Arc::new(InMemoryIndex::new()),
            last_totals: Arc::new(Mutex::new(LiveTotals::new())),
        }
    }
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
    let app2 = app.clone();

    if let Err(e) = thread::Builder::new()
        .name("trashradar-scan".into())
        .spawn(move || {
            // Свіжий live-стан на старті сесії (T-055).
            if let Ok(mut live) = last_totals.lock() {
                live.clear();
            }
            let farm = mvp_predicate_registry();
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
                    // 3) live totals + throttled UI events (≤10/с)
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

#[cfg(test)]
mod tests {
    use super::*;
    use trashradar_app::scan_control::SteppedTestScanner;
    use trashradar_domain::scan::ScanStrategy;

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
