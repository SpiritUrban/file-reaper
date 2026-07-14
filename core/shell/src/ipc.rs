//! Канал команд UI→Core (T-004).
//!
//! Правила:
//! - імена: контракт `namespace.dot` → функція `namespace_dot`
//!   (дзеркальне перетворення — у ui/src/ipc/client.ts);
//! - кожна команда, що може тривати, — `async`: Tauri виконує її на
//!   своєму рантаймі, IPC-потік webview не блокується
//!   (architecture.md §14: усе довше за 16 мс — асинхронне);
//! - помилки — виключно [`CoreError`] (конверт T-007);
//! - реєстрація — один список у [`crate::main`]: нова команда =
//!   функція тут + рядок у generate_handler + запис у contracts/.

use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Runtime};
use trashradar_app::elevation::{
    elevation_benefit_message, elevation_benefit_summary, evaluate_elevation_prompt,
    ElevationSession,
};
use trashradar_app::ports::SettingsSource;
use trashradar_domain::candidate::Decision;
use trashradar_domain::category::CategoryId;
use trashradar_domain::error::CoreError;
use trashradar_domain::settings::{validate_settings, AppSettings};
use trashradar_platform_win::{relaunch_elevated, ElevationRelaunch};

use crate::scan_runtime;

#[derive(Clone)]
pub struct CacheRuntime {
    cache_dir: Option<std::path::PathBuf>,
}

impl CacheRuntime {
    pub fn new(profile: Option<std::path::PathBuf>) -> Self {
        Self {
            cache_dir: profile.map(|path| path.join("cache")),
        }
    }

    fn dir(&self) -> Result<std::path::PathBuf, CoreError> {
        self.cache_dir
            .clone()
            .ok_or_else(|| CoreError::io("LOCALAPPDATA недоступна — cache profile невідомий."))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CacheUsage {
    pub bytes: u64,
    pub files: u64,
}

fn cache_usage(path: &std::path::Path) -> Result<CacheUsage, CoreError> {
    let mut usage = CacheUsage { bytes: 0, files: 0 };
    if !path.exists() {
        return Ok(usage);
    }
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        let metadata = std::fs::symlink_metadata(entry.path())?;
        if metadata.file_type().is_symlink() {
            continue;
        }
        if metadata.is_dir() {
            let nested = cache_usage(&entry.path())?;
            usage.bytes = usage.bytes.saturating_add(nested.bytes);
            usage.files = usage.files.saturating_add(nested.files);
        } else if metadata.is_file() {
            usage.bytes = usage.bytes.saturating_add(metadata.len());
            usage.files = usage.files.saturating_add(1);
        }
    }
    Ok(usage)
}

#[tauri::command]
pub async fn cache_get_usage(
    state: tauri::State<'_, CacheRuntime>,
) -> Result<CacheUsage, CoreError> {
    record_command();
    let dir = state.dir()?;
    tauri::async_runtime::spawn_blocking(move || cache_usage(&dir))
        .await
        .map_err(|error| CoreError::internal(format!("Cache usage task failed: {error}")))?
}

#[tauri::command]
pub async fn cache_clear(state: tauri::State<'_, CacheRuntime>) -> Result<CacheUsage, CoreError> {
    record_command();
    let dir = state.dir()?;
    tauri::async_runtime::spawn_blocking(move || {
        let cleared = cache_usage(&dir)?;
        if dir.exists() {
            for entry in std::fs::read_dir(&dir)? {
                let entry = entry?;
                let metadata = std::fs::symlink_metadata(entry.path())?;
                if metadata.is_dir() && !metadata.file_type().is_symlink() {
                    std::fs::remove_dir_all(entry.path())?;
                } else {
                    std::fs::remove_file(entry.path())?;
                }
            }
        }
        std::fs::create_dir_all(&dir)?;
        Ok(cleared)
    })
    .await
    .map_err(|error| CoreError::internal(format!("Cache clear task failed: {error}")))?
}

#[derive(Clone)]
pub struct SettingsRuntime {
    source: Option<trashradar_settings_json::JsonSettingsSource>,
    current: Arc<RwLock<AppSettings>>,
    schedule_generation: Arc<AtomicU64>,
}

impl SettingsRuntime {
    pub fn new(profile: Option<std::path::PathBuf>) -> Self {
        let source = profile.map(trashradar_settings_json::JsonSettingsSource::in_profile);
        let current = source
            .as_ref()
            .and_then(|source| source.load().ok())
            .unwrap_or_default();
        Self {
            source,
            current: Arc::new(RwLock::new(current)),
            schedule_generation: Arc::new(AtomicU64::new(0)),
        }
    }

    fn source(&self) -> Result<trashradar_settings_json::JsonSettingsSource, CoreError> {
        self.source
            .clone()
            .ok_or_else(|| CoreError::io("LOCALAPPDATA недоступна — settings profile невідомий."))
    }

    pub fn current(&self) -> AppSettings {
        self.current.read().expect("settings lock").clone()
    }
}

#[tauri::command]
pub async fn settings_get(
    state: tauri::State<'_, SettingsRuntime>,
) -> Result<AppSettings, CoreError> {
    record_command();
    Ok(state.current())
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsChangedEvent {
    pub settings: AppSettings,
    pub detector_records_recalculated: u64,
    pub quarantine_rescheduled: bool,
    pub schedule_generation: u64,
}

#[tauri::command]
pub async fn settings_set<R: Runtime>(
    app: AppHandle<R>,
    settings: AppSettings,
    state: tauri::State<'_, SettingsRuntime>,
    scan: tauri::State<'_, crate::scan_runtime::ScanRuntime>,
) -> Result<AppSettings, CoreError> {
    record_command();
    apply_and_persist_settings(&app, &state, &scan, settings).await
}

/// Валідація + атомарний запис + гарячий перерахунок + подія `settings.changed`
/// (T-092/T-093). Спільний шлях для `settings.set` і `category.set_threshold`
/// (T-115) — редагування порога детектора це теж зміна settings, лише з
/// іншим UI-входом.
async fn apply_and_persist_settings<R: Runtime>(
    app: &AppHandle<R>,
    state: &SettingsRuntime,
    scan: &crate::scan_runtime::ScanRuntime,
    settings: AppSettings,
) -> Result<AppSettings, CoreError> {
    validate_settings(&settings).map_err(|error| {
        CoreError::invalid_argument(format!("Поле {}: {}.", error.field, error.message))
    })?;
    let source = state.source()?;
    let saved = settings.clone();
    tauri::async_runtime::spawn_blocking(move || source.save(&saved))
        .await
        .map_err(|error| CoreError::internal(format!("Settings write task failed: {error}")))??;
    let previous = state.current();
    let recalculated = scan.apply_settings(&settings)?;
    let rescheduled = previous.quarantine != settings.quarantine;
    let generation = if rescheduled {
        state.schedule_generation.fetch_add(1, Ordering::SeqCst) + 1
    } else {
        state.schedule_generation.load(Ordering::SeqCst)
    };
    *state.current.write().expect("settings lock") = settings.clone();
    events::emit(
        app,
        events::topic::SETTINGS_CHANGED,
        &SettingsChangedEvent {
            settings: settings.clone(),
            detector_records_recalculated: recalculated,
            quarantine_rescheduled: rescheduled,
            schedule_generation: generation,
        },
    );
    Ok(settings)
}

use crate::events;

/// Живе заповнення тому для блоку дисків Sidebar (T-106).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VolumeUsageInfo {
    /// Літера тому з двокрапкою, напр. `"C:"`.
    pub volume: String,
    pub capacity_bytes: u64,
    pub free_bytes: u64,
}

/// Заповнення всіх готових томів (том без носія пропускається).
pub fn build_volume_usage() -> Vec<VolumeUsageInfo> {
    trashradar_platform_win::list_drive_letters()
        .into_iter()
        .filter_map(|letter| {
            trashradar_platform_win::volume_usage(letter)
                .ok()
                .flatten()
                .map(|usage| VolumeUsageInfo {
                    volume: usage.volume,
                    capacity_bytes: usage.capacity_bytes,
                    free_bytes: usage.free_bytes,
                })
        })
        .collect()
}

/// Бейдж Quarantine у Sidebar (T-106): скільки зараз тримається в карантині.
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct QuarantineBadge {
    pub held_count: u64,
    pub held_bytes: u64,
    /// Дата наступного автоочищення (найраніше expires_at_unix) у UNIX-секундах, або 0 якщо карантин порожний (T-113).
    pub next_purge_at_unix: i64,
}

/// Підсумок manifest-записів зі статусом `Quarantined` (T-078 → бейдж T-106).
pub fn quarantine_badge(
    manifest: &impl trashradar_app::ports::QuarantineManifest,
) -> Result<QuarantineBadge, CoreError> {
    let mut badge = QuarantineBadge {
        held_count: 0,
        held_bytes: 0,
        next_purge_at_unix: 0,
    };
    for entry in manifest.list_entries()? {
        if entry.status == trashradar_domain::quarantine::QuarantineStatus::Quarantined {
            badge.held_count += 1;
            badge.held_bytes = badge.held_bytes.saturating_add(entry.size.0);
            // Отримуємо найраніше expires_at_unix
            if badge.next_purge_at_unix == 0 || entry.expires_at_unix < badge.next_purge_at_unix {
                badge.next_purge_at_unix = entry.expires_at_unix;
            }
        }
    }
    Ok(badge)
}

/// Профіль даних для команд, що читають manifest (інжектується у main;
/// тести підставляють temp-профіль замість реального `%LOCALAPPDATA%`).
#[derive(Clone)]
pub struct ProfileRuntime {
    profile: Option<std::path::PathBuf>,
}

impl ProfileRuntime {
    pub fn new(profile: Option<std::path::PathBuf>) -> Self {
        Self { profile }
    }

    /// Перевіряє, чи це перший запуск (немає маркера .trashradar у профілі).
    /// На першому запуску поставлює маркер.
    pub fn check_and_mark_first_run(&self) -> bool {
        let Some(profile) = &self.profile else {
            return false;
        };
        let marker = profile.join(".trashradar");
        if marker.exists() {
            return false;
        }
        // Перший запуск: створюємо маркер-файл
        if let Err(e) = std::fs::write(&marker, b"first_run_marker") {
            tracing::warn!("Не вдалося записати маркер першого запуску: {}", e);
        }
        true
    }

    /// Каталог профілю (T-130: `preview_runtime::quarantine_thumbnail`
    /// резолвить сурогатний шлях без доступу до приватного поля цього типу).
    pub fn profile_dir(&self) -> Option<std::path::PathBuf> {
        self.profile.clone()
    }
}

/// Відкрити manifest профілю; недоступний профіль/БД → `None` (деградація,
/// не збій — той самий принцип, що й health-проби T-089). Спільна точка
/// відкриття для бейджа (T-106) і вікна карантину (T-130) — жодного
/// дублювання «open + degrade».
fn open_profile_manifest(
    profile: Option<std::path::PathBuf>,
) -> Option<trashradar_index_sqlite::IndexDatabase> {
    let profile = profile?;
    match trashradar_index_sqlite::IndexDatabase::open_profile(profile) {
        Ok(database) => Some(database),
        Err(error) => {
            tracing::warn!(%error, "Manifest профілю не відкрився");
            None
        }
    }
}

/// Бейдж з профільного manifest; недоступна БД → порожній бейдж (деградація,
/// не збій snapshot — той самий принцип, що й health-проби T-089).
fn read_profile_quarantine_badge(profile: Option<std::path::PathBuf>) -> QuarantineBadge {
    match open_profile_manifest(profile) {
        Some(database) => quarantine_badge(&database).unwrap_or_else(|error| {
            tracing::warn!(%error, "Бейдж Quarantine недоступний");
            QuarantineBadge::default()
        }),
        None => QuarantineBadge::default(),
    }
}

/// Дзеркало `QuarantineEntry` (ui/src/ipc/types.ts) для `quarantine.window` (T-130).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QuarantineEntryDto {
    pub id: u64,
    pub batch_id: u64,
    pub original_path: String,
    pub size_bytes: u64,
    pub quarantined_at: String,
    pub expires_at: String,
    pub status: trashradar_domain::quarantine::QuarantineStatus,
}

fn quarantine_entry_to_dto(
    e: &trashradar_domain::quarantine::QuarantineEntry,
) -> QuarantineEntryDto {
    QuarantineEntryDto {
        id: e.id.0,
        batch_id: e.batch_id.map(|b| b.0).unwrap_or(0),
        original_path: e.original_path.clone(),
        size_bytes: e.size.0,
        quarantined_at: unix_secs_to_iso8601(e.quarantined_at_unix.max(0) as u32),
        expires_at: unix_secs_to_iso8601(e.expires_at_unix.max(0) as u32),
        status: e.status,
    }
}

/// Вміст журналу карантину (T-130): лише статус `Quarantined` (той самий
/// фільтр, що й бейдж T-106) — не покинуто in_flight/restored/purged.
/// Сортовано за найближчим автознищенням (wireframe ui.md §7) як дефолт;
/// решта критеріїв (розмір/шлях/дата) — клієнтський пікер, T-131.
fn read_profile_quarantine_entries(profile: Option<std::path::PathBuf>) -> Vec<QuarantineEntryDto> {
    use trashradar_app::ports::QuarantineManifest;
    let Some(database) = open_profile_manifest(profile) else {
        return Vec::new();
    };
    let mut entries = database.list_entries().unwrap_or_else(|error| {
        tracing::warn!(%error, "Вікно Quarantine: manifest недоступний");
        Vec::new()
    });
    entries.retain(|e| e.status == trashradar_domain::quarantine::QuarantineStatus::Quarantined);
    entries.sort_by_key(|e| e.expires_at_unix);
    entries.iter().map(quarantine_entry_to_dto).collect()
}

/// `quarantine.window` (T-130): плитки для екрана Quarantine.
#[tauri::command]
pub async fn quarantine_window(
    profile: tauri::State<'_, ProfileRuntime>,
) -> Result<Vec<QuarantineEntryDto>, CoreError> {
    record_command();
    let profile_dir = profile.profile_dir();
    tauri::async_runtime::spawn_blocking(move || read_profile_quarantine_entries(profile_dir))
        .await
        .map_err(|error| CoreError::internal(format!("Quarantine window task failed: {error}")))
}

/// Один запис за id (T-130: `preview_runtime::quarantine_thumbnail` резолвить
/// сурогатний шлях перед генерацією мініатюри) — не фільтрує за статусом,
/// на відміну від `read_profile_quarantine_entries` (список екрана). Відкриває
/// manifest на кожен виклик; батч (T-132 `restore_profile_quarantine_entries`)
/// відкриває один раз і реюзає `find_quarantine_entry_in`.
pub(crate) fn find_quarantine_entry(
    profile: Option<std::path::PathBuf>,
    entry_id: u64,
) -> Result<trashradar_domain::quarantine::QuarantineEntry, CoreError> {
    let database = open_profile_manifest(profile)
        .ok_or_else(|| CoreError::invalid_argument("Профіль Quarantine недоступний."))?;
    find_quarantine_entry_in(&database, entry_id)
}

/// Параметри `quarantine.restore_batch` (T-132: клавіша R/кнопка шле один
/// entryId; масовий undo батчу — T-081, той самий use case).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QuarantineRestoreBatchPayload {
    pub entry_ids: Vec<u64>,
}

/// Дзеркало `RestoreOutcome` (T-080) для UI: тост «Відновлено у … [Показати]».
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QuarantineRestoredDto {
    pub entry_id: u64,
    pub original_path: String,
    pub restored_path: String,
    pub used_suffix: bool,
}

/// `quarantine.restore_batch` (T-132): відновити один чи кілька записів на
/// оригінальний шлях. Use case (`QuarantineRestorer`, T-080) вже вміє все —
/// FS-move + manifest→Restored + аудит; тут лише резолв сурогатного шляху
/// (той самий примітив, що й `quarantine.thumbnail`, T-130) і подія-міст.
#[tauri::command]
pub async fn quarantine_restore_batch<R: Runtime>(
    app: AppHandle<R>,
    payload: QuarantineRestoreBatchPayload,
    profile: tauri::State<'_, ProfileRuntime>,
) -> Result<Vec<QuarantineRestoredDto>, CoreError> {
    record_command();
    if payload.entry_ids.is_empty() {
        record_command_error();
        return Err(CoreError::invalid_argument("Список entryIds порожній."));
    }
    let profile_dir = profile.profile_dir();
    let entry_ids = payload.entry_ids.clone();
    let outcomes = tauri::async_runtime::spawn_blocking(move || {
        restore_profile_quarantine_entries(profile_dir, &entry_ids)
    })
    .await
    .map_err(|error| CoreError::internal(format!("Quarantine restore task failed: {error}")))?;
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
        dtos.push(QuarantineRestoredDto {
            entry_id: outcome.entry.id.0,
            original_path: outcome.entry.original_path.clone(),
            restored_path: outcome.restored_path.clone(),
            used_suffix: outcome.used_suffix,
        });
    }
    Ok(dtos)
}

fn restore_profile_quarantine_entries(
    profile: Option<std::path::PathBuf>,
    entry_ids: &[u64],
) -> Result<Vec<trashradar_app::RestoreOutcome>, CoreError> {
    use trashradar_domain::quarantine::QuarantineEntryId;

    let database = open_profile_manifest(profile)
        .ok_or_else(|| CoreError::invalid_argument("Профіль Quarantine недоступний."))?;
    let filesystem = trashradar_quarantine_fs::NativeQuarantineFs;
    let restorer = trashradar_app::QuarantineRestorer::new(&filesystem, &database);

    let mut outcomes = Vec::with_capacity(entry_ids.len());
    for &id in entry_ids {
        let entry = find_quarantine_entry_in(&database, id)?;
        let surrogate = filesystem.surrogate_path(&entry.original_path, &entry.surrogate_name)?;
        outcomes.push(restorer.restore_one(trashradar_app::RestoreRequest {
            entry_id: QuarantineEntryId(id),
            surrogate_path: surrogate.to_string_lossy().into_owned(),
        })?);
    }
    Ok(outcomes)
}

/// Той самий лукап, що й `find_quarantine_entry`, але над уже відкритою
/// `database` (батч відновлення не відкриває manifest на кожен запис).
fn find_quarantine_entry_in(
    database: &trashradar_index_sqlite::IndexDatabase,
    entry_id: u64,
) -> Result<trashradar_domain::quarantine::QuarantineEntry, CoreError> {
    use trashradar_app::ports::QuarantineManifest;
    database
        .get_entry(trashradar_domain::quarantine::QuarantineEntryId(entry_id))?
        .ok_or_else(|| {
            CoreError::invalid_argument(format!("Запис Quarantine {entry_id} не знайдено."))
        })
}

/// Параметри `quarantine.reveal_path`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuarantineRevealPathPayload {
    pub path: String,
}

/// «Показати» на тості відновлення (T-132, docs/ui.md §7): відкрити Explorer
/// із виділеним файлом за вже відомим шляхом. Той самий примітив, що й
/// `candidate.reveal_in_explorer` (T-125), але без лукапу за candidateId —
/// щойно відновлений файл не обов'язково є кандидатом у HotIndex.
#[tauri::command]
pub fn quarantine_reveal_path(payload: QuarantineRevealPathPayload) -> Result<(), CoreError> {
    record_command();
    if let Err(e) = trashradar_platform_win::reveal_in_explorer(&payload.path) {
        record_command_error();
        return Err(e);
    }
    Ok(())
}

/// Параметри `quarantine.purge` (T-133): `entryIds` — вибіркове знищення
/// («Знищити позначені»); відсутнє/`null` — «Спорожнити все»
/// (`ManualPurgeSelection::All`, T-083).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QuarantinePurgePayload {
    #[serde(default)]
    pub entry_ids: Option<Vec<u64>>,
}

/// Відповідь `quarantine.purge`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QuarantinePurgeAck {
    pub purged_count: u64,
    pub purged_bytes: u64,
}

/// `quarantine.purge` (T-133, docs/ui.md §7): остаточне видалення — єдине
/// місце в продукті з жорстким підтвердженням (UI, не тут). Use case
/// (`ManualPurger`, T-083) уже вміє все — валідація вибору, фізичне видалення,
/// manifest→Purged, аудит; тут лише резолв сурогатного шляху й подія
/// `quarantine.changed` для живого бейджа Sidebar (T-106) — той самий payload,
/// що й TTL-sweeper (T-082), лише інше джерело виклику.
#[tauri::command]
pub async fn quarantine_purge<R: Runtime>(
    app: AppHandle<R>,
    payload: QuarantinePurgePayload,
    profile: tauri::State<'_, ProfileRuntime>,
) -> Result<QuarantinePurgeAck, CoreError> {
    record_command();
    let profile_dir = profile.profile_dir();
    let profile_dir_for_badge = profile_dir.clone();
    let entry_ids = payload.entry_ids;
    let result = tauri::async_runtime::spawn_blocking(move || {
        purge_profile_quarantine_entries(profile_dir, entry_ids)
    })
    .await
    .map_err(|error| CoreError::internal(format!("Quarantine purge task failed: {error}")))?;
    let result = match result {
        Ok(r) => r,
        Err(error) => {
            record_command_error();
            return Err(error);
        }
    };

    let held = tauri::async_runtime::spawn_blocking(move || {
        read_profile_quarantine_badge(profile_dir_for_badge)
    })
    .await
    .map_err(|error| CoreError::internal(format!("Quarantine badge task failed: {error}")))?;

    events::emit(
        &app,
        events::topic::QUARANTINE_CHANGED,
        &events::QuarantineChangedEvent {
            purged_count: result.purged.len() as u64,
            purged_bytes: result.purged_bytes,
            held_bytes: held.held_bytes,
            threshold_exceeded: false,
            message: None,
        },
    );

    Ok(QuarantinePurgeAck {
        purged_count: result.purged.len() as u64,
        purged_bytes: result.purged_bytes,
    })
}

fn purge_profile_quarantine_entries(
    profile: Option<std::path::PathBuf>,
    entry_ids: Option<Vec<u64>>,
) -> Result<trashradar_app::ManualPurgeResult, CoreError> {
    use trashradar_domain::quarantine::QuarantineEntryId;

    let database = open_profile_manifest(profile)
        .ok_or_else(|| CoreError::invalid_argument("Профіль Quarantine недоступний."))?;
    let filesystem = trashradar_quarantine_fs::NativeQuarantineFs;
    let purger = trashradar_app::ManualPurger::new(&filesystem, &database);

    let selection = match entry_ids {
        Some(ids) => trashradar_app::ManualPurgeSelection::Entries(
            ids.into_iter().map(QuarantineEntryId).collect(),
        ),
        None => trashradar_app::ManualPurgeSelection::All,
    };

    purger.purge(selection, |entry| {
        filesystem
            .surrogate_path(&entry.original_path, &entry.surrogate_name)
            .map(|p| p.to_string_lossy().into_owned())
    })
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppStateSnapshot {
    pub cleanup: events::CleanupTotalEvent,
    pub scan_running: bool,
    pub settings: AppSettings,
    /// Живе заповнення томів для смужок дисків (T-106).
    pub volumes: Vec<VolumeUsageInfo>,
    /// Поточний вміст карантину для бейджа (T-106).
    pub quarantine: QuarantineBadge,
    /// Перший запуск: чистий профіль без попередніх сканів (T-114).
    pub is_first_run: bool,
}

/// Authoritative UI snapshot for webview reload (T-098). Read-only: never starts a scan.
#[tauri::command]
pub async fn app_state(
    scan: tauri::State<'_, crate::scan_runtime::ScanRuntime>,
    settings: tauri::State<'_, SettingsRuntime>,
    profile: tauri::State<'_, ProfileRuntime>,
) -> Result<AppStateSnapshot, CoreError> {
    record_command();
    let cleanup = {
        let totals = scan
            .last_totals
            .lock()
            .map_err(|_| CoreError::internal("Live totals lock poisoned."))?;
        events::CleanupTotalEvent::from_summary(&totals.summary())
    };
    // Дисковий I/O (SQLite manifest) — поза IPC-потоком (§14).
    let manifest_profile = profile.profile.clone();
    let quarantine = tauri::async_runtime::spawn_blocking(move || {
        read_profile_quarantine_badge(manifest_profile)
    })
    .await
    .map_err(|error| CoreError::internal(format!("Quarantine badge task failed: {error}")))?;
    let is_first_run = profile.check_and_mark_first_run();
    Ok(AppStateSnapshot {
        cleanup,
        scan_running: scan.controller.is_running(),
        settings: settings.current(),
        volumes: build_volume_usage(),
        quarantine,
        is_first_run,
    })
}

static COMMANDS_RECEIVED: AtomicU64 = AtomicU64::new(0);
static COMMAND_ERRORS: AtomicU64 = AtomicU64::new(0);

/// Сесійна відмова від elevation (T-034): один процес = одна сесія.
static ELEVATION_DECLINED: AtomicBool = AtomicBool::new(false);

fn elevation_session() -> ElevationSession {
    let mut s = ElevationSession::new();
    if ELEVATION_DECLINED.load(Ordering::Relaxed) {
        s.decline();
    }
    s
}

pub(crate) fn record_command() {
    COMMANDS_RECEIVED.fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn record_command_error() {
    COMMAND_ERRORS.fetch_add(1, Ordering::Relaxed);
}

/// Module status row for the diagnostic health screen (T-009).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModuleHealth {
    pub name: &'static str,
    pub status: &'static str,
}

/// Live IPC counters for the diagnostic health screen (T-009).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IpcMetrics {
    pub commands_received: u64,
    pub command_errors: u64,
    pub events_emitted: u64,
    pub event_errors: u64,
}

/// План стратегії скану для одного тому (T-028) — видно у health.
#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct VolumeScanPlan {
    /// Літера тому з двокрапкою, напр. `"C:"`.
    pub volume: String,
    /// `mft` | `directory_walk` | `usn_delta`.
    pub strategy: String,
    /// `ntfs_elevated` | `not_ntfs` | `not_elevated`.
    pub reason: String,
    /// Ім'я FS або `"unknown"`.
    pub file_system: String,
    pub elevated: bool,
}

/// Доступність карантину на томі (T-089) — видно у health; UI ховає
/// кнопку reap для файлів томів з `available=false` і показує `reason`.
#[derive(Debug, Serialize, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct VolumeQuarantineInfo {
    /// Літера тому з двокрапкою, напр. `"C:"`.
    pub volume: String,
    /// Службовий каталог створено і він реально записуваний → reap дозволений.
    pub available: bool,
    /// Людське пояснення блокування reap (лише коли `available=false`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Сесійний стан elevation для health/UI (T-034).
#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ElevationInfo {
    /// `elevated` | `not_needed` | `offer` | `declined`.
    pub status: String,
    /// Чи UI має показувати банер з поясненням (активна пропозиція).
    pub offer_pending: bool,
    /// Користувач відмовився в цій сесії (DoD: без повторних запитів).
    pub declined_this_session: bool,
    /// Пояснення вигоди (українською).
    pub message: String,
    /// Короткий підсумок для компактних UI.
    pub summary: String,
    /// Скільки NTFS-томів виграли б від MFT.
    pub ntfs_volume_count: u32,
}

/// Відповідь `app.health` — використовується діагностикою (T-009, T-028, T-034).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthInfo {
    pub app_version: &'static str,
    pub core_status: &'static str,
    pub modules: Vec<ModuleHealth>,
    pub ipc: IpcMetrics,
    /// Процес запущено з адмін-правами (T-028).
    pub elevated: bool,
    /// Автовибір MFT ↔ walk по томах (T-028 DoD: видно у health).
    pub scan_plans: Vec<VolumeScanPlan>,
    /// Пропозиція / відмова elevation (T-034).
    pub elevation: ElevationInfo,
    /// Доступність карантину по томах (T-089 DoD: причина видима у UI).
    pub quarantine_volumes: Vec<VolumeQuarantineInfo>,
}

/// Відповідь `app.request_elevation` (T-034).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestElevationReply {
    /// `started` | `already_elevated`.
    pub status: String,
    /// Якщо `started` — поточний процес завершується після відповіді.
    pub will_exit: bool,
}

/// Відповідь `app.decline_elevation` (T-034).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeclineElevationReply {
    pub declined: bool,
    /// Після decline offer більше не pending.
    pub offer_pending: bool,
}

/// Будує плани скану для всіх видимих томів (T-028).
pub fn build_scan_plans() -> (bool, Vec<VolumeScanPlan>) {
    use trashradar_app::ports::ScanEnvironment;
    use trashradar_app::scan_strategy::{choose_scan_strategy, VolumeCapabilities};
    use trashradar_platform_win::WinScanEnvironment;

    let env = WinScanEnvironment;
    let elevated = env.is_elevated();
    let mut plans = Vec::new();

    for volume in env.list_scan_volumes() {
        let file_system = env
            .file_system_name(volume)
            .ok()
            .flatten()
            .unwrap_or_else(|| "unknown".to_string());
        let is_ntfs = file_system.eq_ignore_ascii_case("NTFS");
        // Недоступний том (unknown + not ntfs) → walk / not_ntfs — чесно.
        let choice = choose_scan_strategy(&VolumeCapabilities {
            is_ntfs,
            is_elevated: elevated,
        });
        plans.push(VolumeScanPlan {
            volume: format!("{}:", volume.to_ascii_uppercase()),
            strategy: choice.strategy.as_str().to_string(),
            reason: choice.reason.as_str().to_string(),
            file_system,
            elevated,
        });
    }

    (elevated, plans)
}

/// Кеш capability-проб карантину: health политься щосекунди, а проба —
/// це реальний write-probe на кожному томі (T-077); TTL уникає постійних
/// записів на диск, лишаючи діагностику достатньо свіжою.
type QuarantineCapabilitySnapshot = (Instant, Vec<char>, Vec<VolumeQuarantineInfo>);
static QUARANTINE_CAPABILITY_CACHE: Mutex<Option<QuarantineCapabilitySnapshot>> = Mutex::new(None);
static QUARANTINE_PROBE_IN_FLIGHT: AtomicBool = AtomicBool::new(false);
const QUARANTINE_CAPABILITY_TTL: Duration = Duration::from_secs(30);

/// Синхронна проба доступності карантину томів (T-089, без кешу).
///
/// Успішна проба = каталог `<том>\.trashradar\quarantine` існує/створено
/// і реально записуваний; інакше `available=false` + людська причина
/// (read-only том, брак прав тощо) зі стабільним кодом `quarantine_unavailable`.
pub fn probe_quarantine_volumes(volumes: &[char]) -> Vec<VolumeQuarantineInfo> {
    use trashradar_quarantine_fs::NativeQuarantineFs;

    volumes
        .iter()
        .map(|&volume| {
            let label = format!("{}:", volume.to_ascii_uppercase());
            match NativeQuarantineFs.capability_on_volume(volume) {
                Ok(_) => VolumeQuarantineInfo {
                    volume: label,
                    available: true,
                    reason: None,
                },
                Err(error) => {
                    tracing::warn!(volume = %label, reason = %error.message, "карантин недоступний — reap заблоковано");
                    VolumeQuarantineInfo {
                        volume: label,
                        available: false,
                        reason: Some(error.message),
                    }
                }
            }
        })
        .collect()
}

/// Доступність карантину для health (T-089), stale-while-revalidate.
///
/// Health мусить відповідати миттєво (§14, тест неблокування T-004), а проба —
/// реальний запис на кожен том, тож команда повертає останній знімок одразу;
/// протухлий за TTL знімок оновлюється у фоновому потоці. До завершення першої
/// проби список порожній — UI показує «перевіряється» наступним тіком.
pub fn build_quarantine_volumes(volumes: &[char]) -> Vec<VolumeQuarantineInfo> {
    let snapshot = {
        let cache = QUARANTINE_CAPABILITY_CACHE
            .lock()
            .expect("quarantine capability cache mutex poisoned");
        cache
            .as_ref()
            .and_then(|(probed_at, probed_volumes, infos)| {
                (probed_volumes == volumes).then(|| (probed_at.elapsed(), infos.clone()))
            })
    };
    let (needs_refresh, result) = match snapshot {
        Some((age, infos)) => (age >= QUARANTINE_CAPABILITY_TTL, infos),
        None => (true, Vec::new()),
    };
    if needs_refresh && !QUARANTINE_PROBE_IN_FLIGHT.swap(true, Ordering::AcqRel) {
        let volumes = volumes.to_vec();
        std::thread::spawn(move || {
            let infos = probe_quarantine_volumes(&volumes);
            *QUARANTINE_CAPABILITY_CACHE
                .lock()
                .expect("quarantine capability cache mutex poisoned") =
                Some((Instant::now(), volumes, infos));
            QUARANTINE_PROBE_IN_FLIGHT.store(false, Ordering::Release);
        });
    }
    result
}

/// Зібрати [`ElevationInfo`] з планів і сесійного стану (T-034).
pub fn build_elevation_info(elevated: bool, scan_plans: &[VolumeScanPlan]) -> ElevationInfo {
    let ntfs_volume_count = scan_plans
        .iter()
        .filter(|p| p.file_system.eq_ignore_ascii_case("NTFS"))
        .count() as u32;
    let session = elevation_session();
    let kind = evaluate_elevation_prompt(elevated, ntfs_volume_count > 0, session);
    ElevationInfo {
        status: kind.as_str().to_string(),
        offer_pending: kind.offer_pending(),
        declined_this_session: session.is_declined(),
        message: elevation_benefit_message().to_string(),
        summary: elevation_benefit_summary().to_string(),
        ntfs_volume_count,
    }
}

#[tauri::command]
pub fn app_health() -> HealthInfo {
    record_command();
    tracing::debug!("запит app.health");
    let event_metrics = events::metrics();
    let (elevated, scan_plans) = build_scan_plans();
    let elevation = build_elevation_info(elevated, &scan_plans);
    let volume_letters: Vec<char> = scan_plans
        .iter()
        .filter_map(|plan| plan.volume.chars().next())
        .collect();
    let quarantine_volumes = build_quarantine_volumes(&volume_letters);
    // T-089: підсистема карантину «жива», якщо хоч один том готовий до reap.
    let quarantine_status = if quarantine_volumes.iter().any(|q| q.available) {
        "online"
    } else if quarantine_volumes.is_empty() {
        "planned"
    } else {
        "degraded"
    };
    // T-028: автовибір готовий; деталі стратегії — у scan_plans, не в status.
    HealthInfo {
        app_version: env!("CARGO_PKG_VERSION"),
        core_status: "skeleton",
        modules: vec![
            ModuleHealth {
                name: "shell",
                status: "online",
            },
            ModuleHealth {
                name: "ipc.commands",
                status: "online",
            },
            ModuleHealth {
                name: "ipc.events",
                status: "online",
            },
            ModuleHealth {
                // T-031: scan.journal_stale зареєстровано; повний оркестратор — T-033.
                name: "scanner",
                status: if crate::events::scan_event_topics().is_empty() {
                    "planned"
                } else {
                    "online"
                },
            },
            ModuleHealth {
                name: "index",
                status: "planned",
            },
            ModuleHealth {
                name: "quarantine",
                status: quarantine_status,
            },
        ],
        ipc: IpcMetrics {
            commands_received: COMMANDS_RECEIVED.load(Ordering::Relaxed),
            command_errors: COMMAND_ERRORS.load(Ordering::Relaxed),
            events_emitted: event_metrics.emitted,
            event_errors: event_metrics.failed,
        },
        elevated,
        scan_plans,
        elevation,
        quarantine_volumes,
    }
}

/// Запит UAC elevation: relaunch з `runas` (T-034).
///
/// При успіху (`started`) процес завершується після відправки відповіді,
/// щоб не лишалось два вікна. Скасування UAC → `cancelled` (UI може
/// запропонувати decline).
#[tauri::command]
pub fn app_request_elevation() -> Result<RequestElevationReply, CoreError> {
    record_command();
    tracing::info!("запит app.request_elevation");
    match relaunch_elevated() {
        Ok(ElevationRelaunch::AlreadyElevated) => Ok(RequestElevationReply {
            status: "already_elevated".into(),
            will_exit: false,
        }),
        Ok(ElevationRelaunch::Started) => {
            tracing::info!("UAC accepted — elevated process started; exiting current");
            // Дати Tauri відправити IPC-відповідь, потім вийти.
            std::thread::spawn(|| {
                std::thread::sleep(std::time::Duration::from_millis(200));
                std::process::exit(0);
            });
            Ok(RequestElevationReply {
                status: "started".into(),
                will_exit: true,
            })
        }
        Err(err) => {
            record_command_error();
            // Скасування UAC — не decline сесії автоматично: користувач
            // може натиснути «Продовжити без прав» окремо (явний шлях відмови).
            Err(err)
        }
    }
}

/// Відмова від elevation на сесію (T-034 DoD: без повторних запитів).
///
/// Сканування далі йде через directory walk (T-028). Повторний offer
/// з’явиться лише в новому процесі (перезапуск).
#[tauri::command]
pub fn app_decline_elevation() -> Result<DeclineElevationReply, CoreError> {
    record_command();
    tracing::info!("запит app.decline_elevation");
    ELEVATION_DECLINED.store(true, Ordering::Relaxed);

    let (elevated, plans) = build_scan_plans();
    let info = build_elevation_info(elevated, &plans);
    Ok(DeclineElevationReply {
        declined: true,
        offer_pending: info.offer_pending,
    })
}

/// Параметри `app.ping`. Все опційне: `{}` — миттєвий успіх.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PingPayload {
    /// Штучна затримка відповіді — перевірка неблокування UI.
    #[serde(default)]
    pub delay_ms: Option<u64>,
    /// Запросити відмову — перевірка конверта помилок наскрізь.
    #[serde(default)]
    pub fail: bool,
}

/// Відповідь `app.ping`.
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PingReply {
    pub version: String,
    /// Фактично застосована затримка.
    pub delayed_ms: u64,
}

/// Діагностичний ping: живий наскрізний шлях UI→Core→UI.
///
/// `async` — довгий ping не блокує ні IPC-потік, ні інші команди.
#[tauri::command]
pub async fn app_ping(payload: Option<PingPayload>) -> Result<PingReply, CoreError> {
    record_command();
    let payload = payload.unwrap_or_default();
    tracing::debug!(delay_ms = ?payload.delay_ms, fail = payload.fail, "запит app.ping");

    if payload.fail {
        record_command_error();
        return Err(CoreError::invalid_argument(
            "Тестова відмова app.ping (fail=true).",
        ));
    }

    let delayed_ms = payload.delay_ms.unwrap_or(0);
    if delayed_ms > 0 {
        tokio::time::sleep(std::time::Duration::from_millis(delayed_ms)).await;
    }

    Ok(PingReply {
        version: env!("CARGO_PKG_VERSION").to_string(),
        delayed_ms,
    })
}

/// Параметри `app.test_stream`.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TestStreamPayload {
    /// Скільки подій надіслати (1..=1000, дефолт 5).
    #[serde(default)]
    pub count: Option<u32>,
    /// Пауза між подіями, мс (0..=1000, дефолт 50).
    #[serde(default)]
    pub interval_ms: Option<u64>,
}

/// Неблокуюче підтвердження прийняття `app.test_stream`.
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TestStreamAck {
    pub accepted: u32,
}

/// Подія діагностичного потоку (топік `app.test`).
#[derive(Debug, Clone, Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TestEvent {
    pub seq: u32,
    pub of: u32,
}

/// Діагностичний стрім подій: команда підтверджує прийняття ОДРАЗУ,
/// а події `app.test` летять у фоні — модель «команда → потік подій»
/// з architecture.md §1.2 у мініатюрі.
#[tauri::command]
pub async fn app_test_stream<R: Runtime>(
    app: AppHandle<R>,
    payload: Option<TestStreamPayload>,
) -> Result<TestStreamAck, CoreError> {
    record_command();
    let payload = payload.unwrap_or_default();
    let count = payload.count.unwrap_or(5).clamp(1, 1000);
    let interval = std::time::Duration::from_millis(payload.interval_ms.unwrap_or(50).min(1000));
    tracing::debug!(count, ?interval, "запит app.test_stream");

    tauri::async_runtime::spawn(async move {
        let mut counter_throttle =
            events::CounterThrottle::new_at(std::time::Duration::from_millis(100), Instant::now());
        for seq in 1..=count {
            events::emit(&app, events::topic::APP_TEST, &TestEvent { seq, of: count });
            if let Some(snapshot) = counter_throttle.observe(Instant::now(), 1) {
                events::emit(&app, events::topic::APP_TEST_COUNTER, &snapshot);
            }
            if seq < count {
                tokio::time::sleep(interval).await;
            }
        }
        if let Some(snapshot) = counter_throttle.flush(Instant::now()) {
            events::emit(&app, events::topic::APP_TEST_COUNTER, &snapshot);
        }
    });

    Ok(TestStreamAck { accepted: count })
}

/// Парсить рядковий `categoryId` контракту у [`CategoryId`] (спільно для
/// усіх `category.*` команд — один список варіантів, не дублюється).
fn parse_category_id(value: &str) -> Result<CategoryId, CoreError> {
    match value {
        "large_files" => Ok(CategoryId::LargeFiles),
        "old_files" => Ok(CategoryId::OldFiles),
        "forgotten_videos" => Ok(CategoryId::ForgottenVideos),
        "duplicates" => Ok(CategoryId::Duplicates),
        "archives" => Ok(CategoryId::Archives),
        "installers" => Ok(CategoryId::Installers),
        "temp_files" => Ok(CategoryId::TempFiles),
        "app_caches" => Ok(CategoryId::AppCaches),
        "dev_artifacts" => Ok(CategoryId::DevArtifacts),
        _ => Err(CoreError::invalid_argument("невідома категорія")),
    }
}

/// Кандидати категорії (Keep приховано, T-057), за спаданням розміру —
/// спільна вибірка для `category.top_candidates`/`category.all_candidates`/`category.window`.
fn candidates_in_category(
    scan: &scan_runtime::ScanRuntime,
    category_id: CategoryId,
) -> Vec<trashradar_domain::candidate::FileRecord> {
    let mut records: Vec<_> = scan
        .index
        .get_all()
        .into_iter()
        .filter(|r| r.category == category_id && r.decision != Decision::Keep)
        .collect();
    records.sort_by_key(|r| std::cmp::Reverse(r.size.0));
    records
}

/// Параметри `category.top_candidates`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CategoryTopCandidatesPayload {
    pub category_id: String,
    /// Кількість топ-кандидатів (1..=6, дефолт 4).
    #[serde(default)]
    pub limit: Option<usize>,
}

/// Мініпревью кандидата для 4–6 найбільших у ряду (T-111).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CandidatePreviewDto {
    pub id: u64,
    pub kind: String,
    pub size_bytes: u64,
}

/// Вікно кандидатів категорії з топ-файлами за розміром (запит T-111).
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CategoryWindowDto {
    pub category_id: String,
    /// Топ-кандидати за спаданням розміру (4–6 найбільших).
    pub top_candidates: Vec<CandidatePreviewDto>,
}

/// Запит топ-кандидатів категорії для мініпревью у Cleanup Summary (T-111).
#[tauri::command]
pub fn category_top_candidates(
    payload: CategoryTopCandidatesPayload,
    scan: tauri::State<'_, scan_runtime::ScanRuntime>,
) -> Result<CategoryWindowDto, CoreError> {
    record_command();
    let category_id = parse_category_id(&payload.category_id)?;
    let limit = payload.limit.unwrap_or(4).clamp(1, 6);

    Ok(CategoryWindowDto {
        category_id: payload.category_id,
        top_candidates: candidates_in_category(&scan, category_id)
            .into_iter()
            .take(limit)
            .map(|r| CandidatePreviewDto {
                id: r.candidate_id.0,
                kind: format!("{:?}", r.kind),
                size_bytes: r.size.0,
            })
            .collect(),
    })
}

/// Запит ВСІХ кандидатів категорії для позначення всередину — T-112.
#[tauri::command]
pub fn category_all_candidates(
    category_id_str: String,
    scan: tauri::State<'_, scan_runtime::ScanRuntime>,
) -> Result<Vec<CandidatePreviewDto>, CoreError> {
    record_command();
    let category_id = parse_category_id(&category_id_str)?;

    Ok(candidates_in_category(&scan, category_id)
        .into_iter()
        .map(|r| CandidatePreviewDto {
            id: r.candidate_id.0,
            kind: format!("{:?}", r.kind),
            size_bytes: r.size.0,
        })
        .collect())
}

/// Секунди від 1601-01-01 до 1970-01-01 у Windows FILETIME (100 нс тіки),
/// той самий епох, що й `trashradar_index_memory::filetime_to_unix_secs`.
const UNIX_EPOCH_AS_DAYS: i64 = 719_468;

/// UTC ISO 8601 без зовнішніх крейтів (Howard Hinnant `civil_from_days`,
/// коректно для проксимованого григоріанського календаря).
fn unix_secs_to_iso8601(unix_secs: u32) -> String {
    let secs = unix_secs as i64;
    let days = secs.div_euclid(86_400);
    let time_of_day = secs.rem_euclid(86_400);
    let (hour, minute, second) = (
        time_of_day / 3600,
        (time_of_day / 60) % 60,
        time_of_day % 60,
    );

    let z = days + UNIX_EPOCH_AS_DAYS;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { y + 1 } else { y };

    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// Повний кандидат сітки категорії — дзеркало `Candidate` з ui/src/ipc/types.ts (T-115).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CandidateDto {
    pub id: u64,
    pub path: String,
    pub kind: trashradar_domain::candidate::FileKind,
    pub unit: trashradar_domain::candidate::CandidateUnit,
    pub size_bytes: u64,
    pub last_access_at: String,
    /// `null` — Core не зміг прочитати дату створення (напр. FAT-том без
    /// цього поля) — панель деталей (T-125) просто не показує рядок.
    pub created_at: Option<String>,
    pub decision: Decision,
    pub explanation: String,
    /// Інші категорії файла (маркер «також у: …», T-121).
    pub also_in: Vec<CategoryId>,
}

/// FILETIME (може бути `0`/відсутній) → ISO 8601 або `None`.
fn optional_filetime_to_iso8601(
    ticks: Option<trashradar_domain::candidate::FsTimestamp>,
) -> Option<String> {
    let ticks = ticks?.0;
    if ticks == 0 {
        return None;
    }
    let unix = trashradar_index_memory::filetime_to_unix_secs(ticks);
    if unix == 0 {
        return None;
    }
    Some(unix_secs_to_iso8601(unix))
}

fn file_record_to_candidate_dto(
    record: &trashradar_domain::candidate::FileRecord,
    also_in: Vec<CategoryId>,
) -> CandidateDto {
    let last_access_ticks = record
        .accessed_at
        .or(record.modified_at)
        .map(|t| t.0)
        .unwrap_or(0);
    let last_access_unix = trashradar_index_memory::filetime_to_unix_secs(last_access_ticks);
    CandidateDto {
        id: record.candidate_id.0,
        path: record.path.clone(),
        kind: record.kind,
        unit: record.unit,
        size_bytes: record.size.0,
        last_access_at: unix_secs_to_iso8601(last_access_unix),
        created_at: optional_filetime_to_iso8601(record.created_at),
        decision: record.decision,
        explanation: record.explanation.clone(),
        also_in,
    }
}

/// Параметри `category.window`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CategoryWindowPayload {
    pub category_id: String,
}

/// Сітка кандидатів категорії з повними метаданими (T-115). Кандидати
/// сортовані за розміром спаданням; Keep приховано (T-057).
#[tauri::command]
pub fn category_window(
    payload: CategoryWindowPayload,
    scan: tauri::State<'_, scan_runtime::ScanRuntime>,
) -> Result<Vec<CandidateDto>, CoreError> {
    record_command();
    let category_id = parse_category_id(&payload.category_id)?;

    Ok(candidates_in_category(&scan, category_id)
        .iter()
        .map(|record| {
            let also_in = scan.also_in_categories(record.candidate_id, category_id);
            file_record_to_candidate_dto(record, also_in)
        })
        .collect())
}

/// Параметри `candidate.batch`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CandidateBatchPayload {
    pub candidate_ids: Vec<u64>,
}

/// `candidate.batch` (T-135): повні дані довільного набору кандидатів,
/// **не** обмежених однією категорією — Reap Bar (T-108) і `selectionStore`
/// тримають лише candidateId+sizeBytes на всю сесію (позначення комбінується
/// з кількох категорій), тож оверлей підтвердження REAP потребує окремого
/// способу дістати path/kind/decision саме для позначених id. Той самий
/// лукап по HotIndex, що й `find_record`/`candidates_in_category`, лише без
/// фільтра за категорією; невідомі id мовчки випадають (той самий принцип,
/// що category.window: рядок з битим id — не помилка команди).
#[tauri::command]
pub fn candidate_batch(
    payload: CandidateBatchPayload,
    scan: tauri::State<'_, scan_runtime::ScanRuntime>,
) -> Result<Vec<CandidateDto>, CoreError> {
    record_command();
    let wanted: std::collections::HashSet<u64> = payload.candidate_ids.into_iter().collect();
    Ok(scan
        .index
        .get_all()
        .into_iter()
        .filter(|r| wanted.contains(&r.candidate_id.0))
        .map(|record| {
            let also_in = scan.also_in_categories(record.candidate_id, record.category);
            file_record_to_candidate_dto(&record, also_in)
        })
        .collect())
}

/// Категорії предикатних детекторів з редагованими порогами (T-039..042):
/// id категорії == id детектора у [`scan_runtime::configured_registry`].
const THRESHOLD_EDITABLE_CATEGORIES: &[CategoryId] = &[
    CategoryId::LargeFiles,
    CategoryId::OldFiles,
    CategoryId::ForgottenVideos,
    CategoryId::Archives,
];

/// Параметри `category.set_threshold` (T-115 / T-038): поріг детектора
/// категорії, редагований прямо з рядка детектора над сіткою.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CategorySetThresholdPayload {
    pub category_id: String,
    pub key: String,
    pub value: u64,
}

/// Зміна порога детектора з рядка категорії (T-115). Делегує в той самий
/// шлях, що й `settings.set` (T-092/T-093): гарячий перерахунок з індексу
/// без рескану диска, подія `settings.changed` перебудовує сітку.
#[tauri::command]
pub async fn category_set_threshold<R: Runtime>(
    app: AppHandle<R>,
    payload: CategorySetThresholdPayload,
    state: tauri::State<'_, SettingsRuntime>,
    scan: tauri::State<'_, crate::scan_runtime::ScanRuntime>,
) -> Result<AppSettings, CoreError> {
    record_command();
    let category_id = parse_category_id(&payload.category_id)?;
    if !THRESHOLD_EDITABLE_CATEGORIES.contains(&category_id) {
        return Err(CoreError::invalid_argument(format!(
            "Категорія «{}» не має редагованих порогів.",
            payload.category_id
        )));
    }

    let mut settings = state.current();
    settings
        .detectors
        .entry(payload.category_id.clone())
        .or_default()
        .thresholds
        .insert(payload.key.clone(), payload.value);

    apply_and_persist_settings(&app, &state, &scan, settings).await
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use serde_json::json;
    use tauri::ipc::{CallbackFn, InvokeBody, InvokeResponseBody};
    use tauri::test::{get_ipc_response, mock_builder, mock_context, noop_assets, INVOKE_KEY};
    use tauri::webview::InvokeRequest;
    use tauri::WebviewWindowBuilder;
    use trashradar_app::ports::HotIndex;

    use super::*;

    /// Застосунок на mock-рантаймі з тим самим списком команд, що й main.
    fn test_app() -> (
        tauri::App<tauri::test::MockRuntime>,
        tauri::WebviewWindow<tauri::test::MockRuntime>,
    ) {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let profile = std::env::temp_dir().join(format!("trashradar-ipc-settings-{nonce}"));
        test_app_in_profile(profile)
    }

    /// Як [`test_app`], але з відомим профілем — для тестів manifest (T-106).
    fn test_app_in_profile(
        profile: std::path::PathBuf,
    ) -> (
        tauri::App<tauri::test::MockRuntime>,
        tauri::WebviewWindow<tauri::test::MockRuntime>,
    ) {
        let cache = CacheRuntime::new(Some(profile.clone()));
        let app = mock_builder()
            .manage(crate::scan_runtime::ScanRuntime::new())
            .manage(SettingsRuntime::new(Some(profile.clone())))
            .manage(cache)
            .manage(ProfileRuntime::new(Some(profile.clone())))
            .manage(crate::preview_runtime::PreviewRuntime::new(Some(profile)))
            .invoke_handler(tauri::generate_handler![
                app_health,
                app_state,
                app_ping,
                app_test_stream,
                settings_get,
                settings_set,
                cache_get_usage,
                cache_clear,
                category_top_candidates,
                category_all_candidates,
                category_window,
                category_set_threshold,
                candidate_batch,
                quarantine_window,
                quarantine_restore_batch,
                quarantine_reveal_path,
                quarantine_purge,
                crate::preview_runtime::preview_thumbnail,
                crate::preview_runtime::preview_scrub_strip,
                crate::preview_runtime::preview_large,
                crate::preview_runtime::quarantine_thumbnail,
                crate::scan_runtime::scan_start,
                crate::scan_runtime::scan_stop,
                crate::scan_runtime::candidate_keep,
                crate::scan_runtime::candidate_mark,
                crate::scan_runtime::candidate_reveal_in_explorer,
                crate::scan_runtime::duplicates_groups,
                crate::scan_runtime::reap_execute,
                crate::scan_runtime::reap_undo_batch,
            ])
            .build(mock_context(noop_assets()))
            .expect("mock app");
        let webview = WebviewWindowBuilder::new(&app, "main", Default::default())
            .build()
            .expect("mock webview");
        (app, webview)
    }

    fn request(cmd: &str, body: serde_json::Value) -> InvokeRequest {
        InvokeRequest {
            cmd: cmd.into(),
            callback: CallbackFn(0),
            error: CallbackFn(1),
            url: "http://tauri.localhost".parse().expect("url"),
            body: InvokeBody::Json(body),
            headers: Default::default(),
            invoke_key: INVOKE_KEY.to_string(),
        }
    }

    fn body_json(body: InvokeResponseBody) -> serde_json::Value {
        match body {
            InvokeResponseBody::Json(raw) => serde_json::from_str(&raw).expect("json"),
            other => panic!("очікували JSON-відповідь, отримали {other:?}"),
        }
    }

    #[test]
    fn health_includes_scan_strategy_plans() {
        let (elevated, plans) = build_scan_plans();
        // На Windows є хоча б один том; elevated — bool без паніки.
        let _ = elevated;
        assert!(
            !plans.is_empty(),
            "очікували плани для томів, отримали порожньо"
        );
        for plan in &plans {
            assert!(
                plan.volume.ends_with(':') && plan.volume.len() == 2,
                "volume format: {}",
                plan.volume
            );
            assert!(
                matches!(
                    plan.strategy.as_str(),
                    "mft" | "directory_walk" | "usn_delta"
                ),
                "strategy: {}",
                plan.strategy
            );
            assert!(
                matches!(
                    plan.reason.as_str(),
                    "ntfs_elevated" | "not_ntfs" | "not_elevated"
                ),
                "reason: {}",
                plan.reason
            );
            // Інваріант T-028: MFT лише при NTFS+elevated.
            if plan.strategy == "mft" {
                assert_eq!(plan.reason, "ntfs_elevated");
                assert!(plan.elevated);
                assert!(plan.file_system.eq_ignore_ascii_case("NTFS"));
            }
            if plan.reason == "ntfs_elevated" {
                assert_eq!(plan.strategy, "mft");
            }
            if !plan.elevated {
                assert_ne!(plan.strategy, "mft");
            }
        }
    }

    /// DoD T-089: доступність карантину по томах видима у health (з причиною
    /// блокування reap для недоступних томів).
    #[test]
    fn health_includes_quarantine_capability_t089() {
        let (_elevated, plans) = build_scan_plans();
        let letters: Vec<char> = plans
            .iter()
            .filter_map(|plan| plan.volume.chars().next())
            .collect();
        assert!(!letters.is_empty(), "на Windows є хоча б один том");

        // Синхронна проба: формат записів і людські причини блокування.
        let infos = probe_quarantine_volumes(&letters);
        assert_eq!(infos.len(), letters.len());
        for info in &infos {
            assert!(
                info.volume.ends_with(':') && info.volume.len() == 2,
                "volume format: {}",
                info.volume
            );
            if info.available {
                assert!(info.reason.is_none(), "доступний том не потребує причини");
            } else {
                assert!(
                    info.reason.as_deref().is_some_and(|r| !r.is_empty()),
                    "заблокований том мусить мати людське пояснення"
                );
            }
        }

        // Health-шлях (stale-while-revalidate): перший виклик неблокуючий і
        // стартує фонову пробу; невдовзі health віддає повний знімок.
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let cached = build_quarantine_volumes(&letters);
            if !cached.is_empty() {
                assert_eq!(cached.len(), letters.len());
                break;
            }
            assert!(
                Instant::now() < deadline,
                "фонова проба карантину не завершилась за 10 с"
            );
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    #[test]
    fn health_includes_elevation_info_t034() {
        let (elevated, plans) = build_scan_plans();
        let info = build_elevation_info(elevated, &plans);
        assert!(
            matches!(
                info.status.as_str(),
                "elevated" | "not_needed" | "offer" | "declined"
            ),
            "status: {}",
            info.status
        );
        assert!(!info.message.is_empty());
        assert!(!info.summary.is_empty());
        // Інваріанти: elevated → status elevated, no offer.
        if elevated {
            assert_eq!(info.status, "elevated");
            assert!(!info.offer_pending);
        }
        // offer_pending лише для status=offer.
        if info.offer_pending {
            assert_eq!(info.status, "offer");
        }
        // З правами — MFT на NTFS (T-034 DoD частина 1).
        if elevated {
            for plan in &plans {
                if plan.file_system.eq_ignore_ascii_case("NTFS") {
                    assert_eq!(plan.strategy, "mft");
                }
            }
        } else {
            // Без прав — walk (T-034 + T-028).
            for plan in &plans {
                assert_ne!(plan.strategy, "mft");
            }
        }
    }

    #[test]
    fn decline_elevation_clears_offer_and_keeps_walk() {
        // DoD T-034: після відмови — walk, без повторного offer у сесії.
        let reply = app_decline_elevation().expect("decline ok");
        assert!(reply.declined);
        assert!(!reply.offer_pending);

        let (elevated, plans) = build_scan_plans();
        let info = build_elevation_info(elevated, &plans);
        assert!(info.declined_this_session);
        assert!(!info.offer_pending);
        if !elevated {
            assert_eq!(info.status, "declined");
            for plan in &plans {
                assert_ne!(
                    plan.strategy, "mft",
                    "після decline без admin — walk, plan={plan:?}"
                );
            }
        }
        // Повторний decline ідемпотентний.
        let again = app_decline_elevation().expect("second decline");
        assert!(again.declined);
        assert!(!again.offer_pending);
    }

    #[test]
    fn app_state_restores_ui_without_starting_scan() {
        let (app, webview) = test_app();
        let response = get_ipc_response(&webview, request("app_state", json!({})))
            .expect("app.state snapshot");
        let snapshot = body_json(response);
        assert_eq!(
            snapshot["scanRunning"], false,
            "snapshot must not start a scan"
        );
        assert_eq!(snapshot["cleanup"]["reclaimableBytes"], 0);
        assert_eq!(snapshot["cleanup"]["uniqueFiles"], 0);
        assert_eq!(
            snapshot["settings"],
            serde_json::to_value(AppSettings::default()).unwrap()
        );
        use tauri::Manager;
        assert!(!app
            .state::<crate::scan_runtime::ScanRuntime>()
            .controller
            .is_running());
    }
    fn quarantine_entry(
        id: u64,
        size: u64,
        status: trashradar_domain::quarantine::QuarantineStatus,
    ) -> trashradar_domain::quarantine::QuarantineEntry {
        trashradar_domain::quarantine::QuarantineEntry {
            id: trashradar_domain::quarantine::QuarantineEntryId(id),
            batch_id: None,
            original_path: format!("C:\\Users\\test\\file-{id}.bin"),
            surrogate_name: format!("{id:016x}"),
            size: trashradar_domain::candidate::ByteSize(size),
            quarantined_at_unix: 1_700_000_000,
            expires_at_unix: 1_700_000_000 + 86_400,
            status,
        }
    }

    /// T-106: бейдж рахує лише записи зі статусом Quarantined.
    #[test]
    fn quarantine_badge_counts_only_quarantined_entries() {
        use trashradar_app::ports::QuarantineManifest;
        use trashradar_domain::quarantine::QuarantineStatus;

        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let profile = std::env::temp_dir().join(format!("trashradar-t106-badge-{nonce}"));
        let database =
            trashradar_index_sqlite::IndexDatabase::open_profile(&profile).expect("manifest db");
        database
            .insert_entry(&quarantine_entry(1, 100, QuarantineStatus::Quarantined))
            .unwrap();
        database
            .insert_entry(&quarantine_entry(2, 250, QuarantineStatus::Quarantined))
            .unwrap();
        database
            .insert_entry(&quarantine_entry(3, 999, QuarantineStatus::Purged))
            .unwrap();
        database
            .insert_entry(&quarantine_entry(4, 777, QuarantineStatus::Restored))
            .unwrap();

        let badge = quarantine_badge(&database).expect("badge");
        assert_eq!(
            badge,
            QuarantineBadge {
                held_count: 2,
                held_bytes: 350,
                next_purge_at_unix: 1_700_000_000 + 86_400,
            }
        );
        drop(database);
        let _ = std::fs::remove_dir_all(profile);
    }

    /// DoD T-130: вікно карантину — лише `Quarantined` (той самий фільтр, що
    /// й бейдж T-106), сортовано за найближчим автознищенням; camelCase-поля
    /// відповідають `ui/src/ipc/types.ts` `QuarantineEntry`.
    #[test]
    fn quarantine_window_returns_only_quarantined_sorted_by_expiry() {
        use trashradar_app::ports::QuarantineManifest;
        use trashradar_domain::candidate::ByteSize;
        use trashradar_domain::quarantine::{
            BatchId, QuarantineEntry, QuarantineEntryId, QuarantineStatus,
        };

        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let profile = std::env::temp_dir().join(format!("trashradar-t130-window-{nonce}"));
        {
            let database = trashradar_index_sqlite::IndexDatabase::open_profile(&profile)
                .expect("manifest db");
            database
                .insert_entry(&QuarantineEntry {
                    id: QuarantineEntryId(1),
                    batch_id: None,
                    original_path: "C:\\Users\\test\\later.bin".into(),
                    surrogate_name: "0000000000000001".into(),
                    size: ByteSize(500),
                    quarantined_at_unix: 1_700_000_000,
                    expires_at_unix: 1_700_200_000,
                    status: QuarantineStatus::Quarantined,
                })
                .unwrap();
            database
                .insert_entry(&QuarantineEntry {
                    id: QuarantineEntryId(2),
                    batch_id: Some(BatchId(9)),
                    original_path: "C:\\Users\\test\\sooner.bin".into(),
                    surrogate_name: "0000000000000002".into(),
                    size: ByteSize(300),
                    quarantined_at_unix: 1_700_000_000,
                    expires_at_unix: 1_700_100_000,
                    status: QuarantineStatus::Quarantined,
                })
                .unwrap();
            database
                .insert_entry(&QuarantineEntry {
                    id: QuarantineEntryId(3),
                    batch_id: None,
                    original_path: "C:\\Users\\test\\gone.bin".into(),
                    surrogate_name: "0000000000000003".into(),
                    size: ByteSize(999),
                    quarantined_at_unix: 1_700_000_000,
                    expires_at_unix: 1_700_050_000,
                    status: QuarantineStatus::Purged,
                })
                .unwrap();
        }

        let (_app, webview) = test_app_in_profile(profile.clone());
        let response = get_ipc_response(&webview, request("quarantine_window", json!({})))
            .expect("quarantine.window");
        let entries = body_json(response);
        let entries = entries.as_array().expect("entries array");

        assert_eq!(entries.len(), 2, "Purged не входить у вікно");
        assert_eq!(entries[0]["id"], 2, "найближче автознищення — перше");
        assert_eq!(entries[0]["batchId"], 9);
        assert_eq!(entries[0]["sizeBytes"], 300);
        assert_eq!(entries[0]["status"], "quarantined");
        assert!(entries[0]["expiresAt"].as_str().unwrap().ends_with('Z'));
        assert_eq!(entries[1]["id"], 1);
        assert_eq!(entries[1]["batchId"], 0, "відсутній batchId у домені → 0");

        let _ = std::fs::remove_dir_all(profile);
    }

    /// DoD T-130: невідомий запис — типізована відмова, не паніка.
    #[test]
    fn quarantine_thumbnail_rejects_unknown_entry() {
        let (_app, webview) = test_app();
        let result = get_ipc_response(
            &webview,
            request(
                "quarantine_thumbnail",
                json!({ "payload": { "entryId": 999 } }),
            ),
        );
        assert!(result.is_err());
    }

    /// DoD T-132: порожній список entryIds — типізована відмова до будь-якого I/O.
    #[test]
    fn quarantine_restore_batch_rejects_empty_entry_ids() {
        let (_app, webview) = test_app();
        let envelope = get_ipc_response(
            &webview,
            request(
                "quarantine_restore_batch",
                json!({ "payload": { "entryIds": [] } }),
            ),
        )
        .expect_err("порожній список мусить відмовити");
        assert_eq!(envelope["code"], "invalid_argument");
    }

    /// DoD T-132: невідомий запис — типізована відмова, не паніка (той самий
    /// принцип, що й `quarantine_thumbnail_rejects_unknown_entry`).
    #[test]
    fn quarantine_restore_batch_rejects_unknown_entry() {
        let (_app, webview) = test_app();
        let result = get_ipc_response(
            &webview,
            request(
                "quarantine_restore_batch",
                json!({ "payload": { "entryIds": [999] } }),
            ),
        );
        assert!(result.is_err());
    }

    /// DoD T-132: порожній шлях — типізована відмова; реальний виклик
    /// `explorer.exe` свідомо не тестується тут (він і справді відкрив би
    /// вікно Провідника під час `cargo test`, той самий принцип, що й T-034/T-125).
    #[test]
    fn quarantine_reveal_path_rejects_empty_path() {
        let (_app, webview) = test_app();
        let envelope = get_ipc_response(
            &webview,
            request(
                "quarantine_reveal_path",
                json!({ "payload": { "path": "" } }),
            ),
        )
        .expect_err("порожній шлях мусить відмовити");
        assert_eq!(envelope["code"], "invalid_argument");
    }

    /// DoD T-133: вибірковий покажчик entryIds:[] — типізована відмова до
    /// будь-якого I/O (та сама валідація use case ManualPurger, T-083).
    #[test]
    fn quarantine_purge_rejects_empty_selective_list() {
        let (_app, webview) = test_app();
        let envelope = get_ipc_response(
            &webview,
            request("quarantine_purge", json!({ "payload": { "entryIds": [] } })),
        )
        .expect_err("порожній вибірковий список мусить відмовити");
        assert_eq!(envelope["code"], "invalid_argument");
    }

    /// DoD T-133: після знищення місце фактично звільняється — сурогат
    /// реально прибирається з диска, manifest переходить у Purged, ack несе
    /// коректні purgedCount/purgedBytes.
    #[test]
    fn quarantine_purge_removes_real_surrogate_and_updates_manifest() {
        use trashradar_app::ports::QuarantineManifest;
        use trashradar_domain::candidate::ByteSize;
        use trashradar_domain::quarantine::{QuarantineEntry, QuarantineEntryId, QuarantineStatus};
        use trashradar_quarantine_fs::NativeQuarantineFs;

        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let profile = std::env::temp_dir().join(format!("trashradar-t133-purge-{nonce}"));

        // Реальний сурогат під C:\.trashradar\quarantine\ — surrogate_path
        // резолвить том з original_path (тут завжди C:, як і решта тестів
        // quarantine-fs/T-079..083 у цьому воркспейсі).
        let directory = NativeQuarantineFs
            .ensure_at_root(std::path::Path::new("C:\\"))
            .expect("quarantine root");
        let surrogate_name = format!("t133-{nonce:x}");
        let surrogate_path = directory.quarantine_root.join(&surrogate_name);
        std::fs::write(&surrogate_path, b"purge me").expect("write surrogate");

        let original_path = format!("C:\\Users\\test\\trashradar-t133-{nonce}.bin");
        {
            let database = trashradar_index_sqlite::IndexDatabase::open_profile(&profile)
                .expect("manifest db");
            database
                .insert_entry(&QuarantineEntry {
                    id: QuarantineEntryId(1),
                    batch_id: None,
                    original_path: original_path.clone(),
                    surrogate_name: surrogate_name.clone(),
                    size: ByteSize(8),
                    quarantined_at_unix: 1_700_000_000,
                    expires_at_unix: 1_700_000_000 + 86_400,
                    status: QuarantineStatus::Quarantined,
                })
                .unwrap();
        }

        let (_app, webview) = test_app_in_profile(profile.clone());
        let response = get_ipc_response(
            &webview,
            request(
                "quarantine_purge",
                json!({ "payload": { "entryIds": [1] } }),
            ),
        )
        .expect("quarantine.purge");
        let ack = body_json(response);
        assert_eq!(ack["purgedCount"], 1);
        assert_eq!(ack["purgedBytes"], 8);
        assert!(
            !surrogate_path.exists(),
            "сурогат мусить бути реально видалений з диска"
        );

        let database =
            trashradar_index_sqlite::IndexDatabase::open_profile(&profile).expect("reopen");
        let entry = database
            .get_entry(QuarantineEntryId(1))
            .unwrap()
            .expect("запис лишається в manifest як історія (append-only)");
        assert_eq!(entry.status, QuarantineStatus::Purged);

        let _ = std::fs::remove_dir_all(profile);
    }

    /// DoD T-138: `reap.execute` реально переміщує файл у Quarantine
    /// (durable in_flight→move→quarantined, use case T-079) і ховає
    /// кандидата з усіх категорій сесії тим самим шляхом, що й Keep (T-057);
    /// `reap.undo_batch` (use case T-081) відновлює весь батч одним викликом.
    #[test]
    fn reap_execute_moves_file_and_undo_batch_restores_it() {
        use tauri::Manager;
        use trashradar_domain::candidate::{
            ByteSize, CandidateId, CandidateUnit, FileAttributes, FileKind, SafetyLevel,
        };

        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let profile = std::env::temp_dir().join(format!("trashradar-t138-profile-{nonce}"));
        let source_dir = std::env::temp_dir().join(format!("trashradar-t138-src-{nonce}"));
        std::fs::create_dir_all(&source_dir).expect("source dir");
        let source_path = source_dir.join("marked.bin");
        let content = b"reap me please";
        std::fs::write(&source_path, content).expect("write source");
        let source_path_str = source_path.to_string_lossy().into_owned();

        let (app, webview) = test_app_in_profile(profile.clone());
        let scan = app.state::<crate::scan_runtime::ScanRuntime>();

        // Реальна mtime файла для гарячого індексу — execute_reap_batch сам
        // звіряє її з живим диском на найкращій точності, яку захопило
        // сканування (розмір точно, mtime до цілої секунди — CompactFileRecord
        // не зберігає більшого), тож тут достатньо будь-якого реального
        // значення; None дав би false-positive file_changed.
        let identity =
            trashradar_platform_win::read_file_identity(&source_path).expect("file identity");
        scan.index
            .insert_batch(vec![trashradar_domain::candidate::FileRecord {
                candidate_id: CandidateId(555_555),
                path: source_path_str.clone(),
                size: ByteSize(content.len() as u64),
                created_at: None,
                modified_at: identity.modified_at,
                accessed_at: None,
                kind: FileKind::Other,
                unit: CandidateUnit::File,
                category: CategoryId::LargeFiles,
                safety: SafetyLevel::ReviewRecommended,
                decision: Decision::Marked,
                detector_id: String::new(),
                explanation: String::new(),
                attributes: FileAttributes::default(),
            }])
            .unwrap();

        let response = get_ipc_response(
            &webview,
            request(
                "reap_execute",
                json!({ "payload": { "candidateIds": [555555] } }),
            ),
        )
        .expect("reap.execute");
        let ack = body_json(response);
        assert_eq!(ack["reapedCount"], 1);
        assert_eq!(ack["reapedBytes"], content.len());
        let batch_id = ack["batchId"].as_u64().expect("batchId");

        assert!(
            !source_path.exists(),
            "файл мусить бути фізично переміщений"
        );
        let records = scan.index.get_all();
        let record = records
            .iter()
            .find(|r| r.candidate_id == CandidateId(555_555))
            .expect("запис лишається в індексі");
        assert_eq!(
            record.decision,
            Decision::Keep,
            "зреапнутий запис приховано тим самим шляхом, що й Keep (T-057)"
        );

        // «Скасувати» — весь батч одним викликом.
        let undo_response = get_ipc_response(
            &webview,
            request(
                "reap_undo_batch",
                json!({ "payload": { "batchId": batch_id } }),
            ),
        )
        .expect("reap.undo_batch");
        let outcomes = body_json(undo_response);
        let outcomes = outcomes.as_array().expect("масив відновлень");
        assert_eq!(outcomes.len(), 1);
        assert_eq!(outcomes[0]["restoredPath"], source_path_str);
        assert!(source_path.exists(), "файл мусить повернутися на місце");

        let _ = std::fs::remove_dir_all(&profile);
        let _ = std::fs::remove_dir_all(&source_dir);
    }

    /// DoD T-138: порожній список candidateIds — типізована відмова до
    /// будь-якого I/O.
    #[test]
    fn reap_execute_rejects_empty_candidate_ids() {
        let (_app, webview) = test_app();
        let envelope = get_ipc_response(
            &webview,
            request("reap_execute", json!({ "payload": { "candidateIds": [] } })),
        )
        .expect_err("порожній список мусить відмовити");
        assert_eq!(envelope["code"], "invalid_argument");
    }

    /// T-106: snapshot містить живі томи (capacity/free) і бейдж карантину.
    #[test]
    fn app_state_includes_volume_usage_and_quarantine_badge() {
        use trashradar_app::ports::QuarantineManifest;
        use trashradar_domain::quarantine::QuarantineStatus;

        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let profile = std::env::temp_dir().join(format!("trashradar-t106-state-{nonce}"));
        {
            let database = trashradar_index_sqlite::IndexDatabase::open_profile(&profile)
                .expect("manifest db");
            database
                .insert_entry(&quarantine_entry(1, 4096, QuarantineStatus::Quarantined))
                .unwrap();
        }

        let (_app, webview) = test_app_in_profile(profile.clone());
        let response = get_ipc_response(&webview, request("app_state", json!({})))
            .expect("app.state snapshot");
        let snapshot = body_json(response);

        assert_eq!(snapshot["quarantine"]["heldCount"], 1);
        assert_eq!(snapshot["quarantine"]["heldBytes"], 4096);

        let volumes = snapshot["volumes"].as_array().expect("volumes array");
        assert!(
            !volumes.is_empty(),
            "на Windows очікуємо хоча б один готовий том"
        );
        for volume in volumes {
            let label = volume["volume"].as_str().expect("volume label");
            assert!(label.ends_with(':') && label.len() == 2, "label: {label}");
            let capacity = volume["capacityBytes"].as_u64().expect("capacity");
            let free = volume["freeBytes"].as_u64().expect("free");
            assert!(capacity > 0, "capacity має бути > 0");
            assert!(free <= capacity, "free ≤ capacity");
        }
        let _ = std::fs::remove_dir_all(profile);
    }

    #[test]
    fn ping_roundtrips_ui_core_ui() {
        let (_app, webview) = test_app();
        let response =
            get_ipc_response(&webview, request("app_ping", json!({}))).expect("успішна відповідь");
        let reply: PingReply = serde_json::from_value(body_json(response)).expect("форма reply");
        assert_eq!(reply.version, env!("CARGO_PKG_VERSION"));
        assert_eq!(reply.delayed_ms, 0);
    }

    #[test]
    fn ping_failure_returns_t007_envelope() {
        let (_app, webview) = test_app();
        let envelope = get_ipc_response(
            &webview,
            request("app_ping", json!({ "payload": { "fail": true } })),
        )
        .expect_err("команда мусить відмовити");
        assert_eq!(envelope["code"], "invalid_argument");
        assert!(envelope["message"].as_str().expect("текст").ends_with('.'));
    }

    #[test]
    fn settings_get_set_roundtrip_over_ipc() {
        let (app, webview) = test_app();
        let defaults =
            get_ipc_response(&webview, request("settings_get", json!({}))).expect("settings.get");
        let mut settings: AppSettings =
            serde_json::from_value(body_json(defaults)).expect("settings shape");
        settings.quarantine.ttl_days = 14;
        let saved = get_ipc_response(
            &webview,
            request("settings_set", json!({ "settings": settings })),
        )
        .expect("settings.set");
        let saved: AppSettings = serde_json::from_value(body_json(saved)).expect("saved shape");
        assert_eq!(saved.quarantine.ttl_days, 14);
        let loaded = get_ipc_response(&webview, request("settings_get", json!({})))
            .expect("settings.get after set");
        let loaded: AppSettings = serde_json::from_value(body_json(loaded)).expect("loaded shape");
        assert_eq!(loaded, saved);
        use tauri::Manager;
        let runtime = app.state::<SettingsRuntime>();
        assert_eq!(runtime.schedule_generation.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn settings_set_rejects_invalid_field_without_persisting() {
        let (_app, webview) = test_app();
        let result = get_ipc_response(
            &webview,
            request(
                "settings_set",
                json!({
                    "settings": {
                        "quarantine": { "ttlDays": 0, "warningThresholdBytes": 1048576 },
                        "scan": { "excludedPaths": [], "minimumSizeBytes": 0 }
                    }
                }),
            ),
        )
        .expect_err("invalid ttl rejected");
        assert_eq!(result["code"], "invalid_argument");
        assert!(result["message"]
            .as_str()
            .unwrap()
            .contains("quarantine.ttlDays"));
        let loaded = get_ipc_response(&webview, request("settings_get", json!({})))
            .expect("settings.get remains available");
        let loaded: AppSettings = serde_json::from_value(body_json(loaded)).unwrap();
        assert_eq!(loaded, AppSettings::default());
    }

    #[test]
    fn cache_usage_and_clear_cover_only_profile_cache() {
        let (app, webview) = test_app();
        use tauri::Manager;
        let runtime = app.state::<CacheRuntime>();
        let cache_dir = runtime.dir().unwrap();
        std::fs::create_dir_all(cache_dir.join("previews")).unwrap();
        std::fs::write(cache_dir.join("previews").join("a.bin"), [1_u8; 7]).unwrap();
        std::fs::write(cache_dir.join("b.bin"), [2_u8; 5]).unwrap();
        let usage = get_ipc_response(&webview, request("cache_get_usage", json!({}))).unwrap();
        let usage: CacheUsage = serde_json::from_value(body_json(usage)).unwrap();
        assert_eq!(
            usage,
            CacheUsage {
                bytes: 12,
                files: 2
            }
        );
        let cleared = get_ipc_response(&webview, request("cache_clear", json!({}))).unwrap();
        let cleared: CacheUsage = serde_json::from_value(body_json(cleared)).unwrap();
        assert_eq!(cleared, usage);
        assert!(cache_dir.exists());
        assert_eq!(
            cache_usage(&cache_dir).unwrap(),
            CacheUsage { bytes: 0, files: 0 }
        );
    }

    #[test]
    fn long_ping_does_not_block_parallel_commands() {
        let (_app, webview) = test_app();
        let started = Instant::now();
        let slow = std::thread::spawn({
            let webview = webview.clone();
            move || {
                get_ipc_response(
                    &webview,
                    request("app_ping", json!({ "payload": { "delayMs": 400 } })),
                )
            }
        });
        // Поки повільний ping спить, швидкі команди мають відповідати.
        let quick = get_ipc_response(&webview, request("app_health", json!({})));
        let quick_elapsed = started.elapsed();
        assert!(quick.is_ok(), "паралельна команда відповіла");
        assert!(
            quick_elapsed.as_millis() < 300,
            "швидка команда не чекала на повільну: {quick_elapsed:?}"
        );
        let slow = slow
            .join()
            .expect("потік")
            .expect("повільний ping успішний");
        let reply: PingReply = serde_json::from_value(body_json(slow)).expect("reply");
        assert_eq!(reply.delayed_ms, 400);
    }

    /// Rust-підписник шини: збирає TestEvent-и у канал.
    fn listen_test_events(
        app: &tauri::App<tauri::test::MockRuntime>,
    ) -> (tauri::EventId, std::sync::mpsc::Receiver<TestEvent>) {
        use tauri::Listener;
        let (tx, rx) = std::sync::mpsc::channel::<TestEvent>();
        let id = app.listen(
            crate::events::wire_name(crate::events::topic::APP_TEST),
            move |event| {
                let parsed: TestEvent = serde_json::from_str(event.payload()).expect("payload");
                let _ = tx.send(parsed);
            },
        );
        (id, rx)
    }

    fn drain_for(
        rx: &std::sync::mpsc::Receiver<TestEvent>,
        window: std::time::Duration,
    ) -> Vec<TestEvent> {
        let deadline = Instant::now() + window;
        let mut received = Vec::new();
        loop {
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            match rx.recv_timeout(deadline - now) {
                Ok(event) => received.push(event),
                Err(_) => break,
            }
        }
        received
    }

    #[test]
    fn test_stream_delivers_all_events_to_subscriber() {
        let (app, webview) = test_app();
        let (_id, rx) = listen_test_events(&app);
        let ack = get_ipc_response(
            &webview,
            request(
                "app_test_stream",
                json!({ "payload": { "count": 5, "intervalMs": 1 } }),
            ),
        )
        .expect("ack");
        let ack: TestStreamAck = serde_json::from_value(body_json(ack)).expect("форма ack");
        assert_eq!(ack.accepted, 5);

        let received = drain_for(&rx, std::time::Duration::from_millis(1500));
        assert_eq!(received.len(), 5, "усі події дійшли: {received:?}");
        assert_eq!(received[0], TestEvent { seq: 1, of: 5 });
        assert_eq!(received[4], TestEvent { seq: 5, of: 5 });
    }

    #[test]
    fn ack_returns_before_stream_completes() {
        let (app, webview) = test_app();
        let (_id, rx) = listen_test_events(&app);
        let started = Instant::now();
        get_ipc_response(
            &webview,
            request(
                "app_test_stream",
                json!({ "payload": { "count": 4, "intervalMs": 150 } }),
            ),
        )
        .expect("ack");
        let ack_elapsed = started.elapsed();
        assert!(
            ack_elapsed.as_millis() < 300,
            "підтвердження прийняття не чекає завершення потоку: {ack_elapsed:?}"
        );
        // Потік завершується вже ПІСЛЯ підтвердження.
        let received = drain_for(&rx, std::time::Duration::from_millis(2000));
        assert_eq!(received.len(), 4);
    }

    #[test]
    fn unlisten_stops_delivery() {
        use tauri::Listener;
        let (app, webview) = test_app();
        let (id, rx) = listen_test_events(&app);

        get_ipc_response(
            &webview,
            request(
                "app_test_stream",
                json!({ "payload": { "count": 2, "intervalMs": 1 } }),
            ),
        )
        .expect("перший потік");
        let first = drain_for(&rx, std::time::Duration::from_millis(1000));
        assert_eq!(first.len(), 2, "до відписки події доходять");

        app.unlisten(id);

        get_ipc_response(
            &webview,
            request(
                "app_test_stream",
                json!({ "payload": { "count": 3, "intervalMs": 1 } }),
            ),
        )
        .expect("другий потік");
        let after = drain_for(&rx, std::time::Duration::from_millis(500));
        assert!(
            after.is_empty(),
            "після відписки доставка зупинена, отримано: {after:?}"
        );
    }

    #[test]
    fn unknown_payload_fields_are_rejected() {
        let (_app, webview) = test_app();
        let result = get_ipc_response(
            &webview,
            request(
                "app_ping",
                json!({ "payload": { "delayMs": 1, "oops": true } }),
            ),
        );
        assert!(result.is_err(), "deny_unknown_fields працює");
    }

    /// Тестовий FileRecord у категорії — для category.window/category.set_threshold (T-115).
    fn sample_file_record(
        id: u64,
        size_bytes: u64,
        category: CategoryId,
        decision: Decision,
    ) -> trashradar_domain::candidate::FileRecord {
        use trashradar_domain::candidate::{
            ByteSize, CandidateId, CandidateUnit, FileAttributes, FileKind, FsTimestamp,
            SafetyLevel,
        };
        trashradar_domain::candidate::FileRecord {
            candidate_id: CandidateId(id),
            path: format!("C:\\test\\file-{id}.bin"),
            size: ByteSize(size_bytes),
            created_at: None,
            modified_at: None,
            accessed_at: Some(FsTimestamp(133_500_000_000_000_000)),
            kind: FileKind::Other,
            unit: CandidateUnit::File,
            category,
            safety: SafetyLevel::ReviewRecommended,
            decision,
            detector_id: "large_files".into(),
            explanation: "розмір понад поріг".into(),
            attributes: FileAttributes::default(),
        }
    }

    #[test]
    fn unix_secs_to_iso8601_known_instant() {
        // 2024-01-15T10:30:00Z → 1705314600 (перевірено незалежним розрахунком).
        assert_eq!(unix_secs_to_iso8601(1_705_314_600), "2024-01-15T10:30:00Z");
        assert_eq!(unix_secs_to_iso8601(0), "1970-01-01T00:00:00Z");
    }

    /// DoD T-125: дата створення в панелі деталей — `None`/`0` чесно
    /// дають `None` (не фальшиве 1970-01-01), реальна дата конвертується.
    #[test]
    fn optional_filetime_to_iso8601_handles_absent_and_present() {
        use trashradar_domain::candidate::FsTimestamp;
        assert_eq!(optional_filetime_to_iso8601(None), None);
        assert_eq!(optional_filetime_to_iso8601(Some(FsTimestamp(0))), None);
        // Той самий FILETIME, що й у sample_file_record (accessed_at) — вже
        // перевірений у category_window-тестах на реальну ISO-дату.
        assert!(
            optional_filetime_to_iso8601(Some(FsTimestamp(133_500_000_000_000_000)))
                .unwrap()
                .ends_with('Z')
        );
    }

    #[test]
    fn category_window_returns_full_candidates_sorted_desc_and_hides_keep() {
        let (app, webview) = test_app();
        use tauri::Manager;
        let scan = app.state::<crate::scan_runtime::ScanRuntime>();
        scan.index
            .insert_batch(vec![
                sample_file_record(
                    1,
                    10 * 1024 * 1024,
                    CategoryId::LargeFiles,
                    Decision::Undecided,
                ),
                sample_file_record(
                    2,
                    50 * 1024 * 1024,
                    CategoryId::LargeFiles,
                    Decision::Undecided,
                ),
                sample_file_record(3, 999 * 1024 * 1024, CategoryId::LargeFiles, Decision::Keep),
                sample_file_record(
                    4,
                    5 * 1024 * 1024,
                    CategoryId::OldFiles,
                    Decision::Undecided,
                ),
            ])
            .unwrap();

        let response = get_ipc_response(
            &webview,
            request(
                "category_window",
                json!({ "payload": { "categoryId": "large_files" } }),
            ),
        )
        .expect("category.window");
        let candidates = body_json(response);
        let list = candidates.as_array().expect("масив кандидатів");
        assert_eq!(list.len(), 2, "Keep приховано, чужа категорія не входить");
        assert_eq!(list[0]["id"], 2, "сортовано за розміром спаданням");
        assert_eq!(list[1]["id"], 1);
        assert_eq!(list[0]["path"], "C:\\test\\file-2.bin");
        assert_eq!(list[0]["kind"], "other");
        assert_eq!(list[0]["unit"], "file");
        assert_eq!(list[0]["decision"], "undecided");
        assert!(list[0]["lastAccessAt"].as_str().unwrap().ends_with('Z'));
        assert_eq!(
            list[0]["createdAt"],
            serde_json::Value::Null,
            "sample_file_record не задає created_at — DTO чесно віддає null, не 1970-01-01"
        );
        assert_eq!(list[0]["alsoIn"], json!([]));
    }

    /// DoD T-135: `candidate.batch` дістає повні дані для позначених id **з
    /// різних категорій** одним запитом (Reap Bar/selectionStore не знають
    /// категорії, лише candidateId); невідомий id мовчки випадає, а не
    /// падає команда; Keep не виключається спеціально (candidate.batch не
    /// категорійна вибірка — рішення відображається як є).
    #[test]
    fn candidate_batch_returns_requested_ids_across_categories() {
        let (app, webview) = test_app();
        use tauri::Manager;
        let scan = app.state::<crate::scan_runtime::ScanRuntime>();
        scan.index
            .insert_batch(vec![
                sample_file_record(
                    1,
                    10 * 1024 * 1024,
                    CategoryId::LargeFiles,
                    Decision::Undecided,
                ),
                sample_file_record(4, 5 * 1024 * 1024, CategoryId::OldFiles, Decision::Marked),
                sample_file_record(5, 1024 * 1024, CategoryId::TempFiles, Decision::Undecided),
            ])
            .unwrap();

        let response = get_ipc_response(
            &webview,
            request(
                "candidate_batch",
                json!({ "payload": { "candidateIds": [1, 4, 999] } }),
            ),
        )
        .expect("candidate.batch");
        let candidates = body_json(response);
        let list = candidates.as_array().expect("масив кандидатів");
        assert_eq!(list.len(), 2, "999 невідомий — мовчки випадає");
        let ids: Vec<u64> = list.iter().map(|c| c["id"].as_u64().unwrap()).collect();
        assert!(ids.contains(&1) && ids.contains(&4));
        assert!(!ids.contains(&5), "5 не запитувався — не входить");
        let marked = list.iter().find(|c| c["id"] == 4).unwrap();
        assert_eq!(marked["decision"], "marked");
    }

    /// DoD T-121: файл, що заслуговує на кілька категорій, несе маркер
    /// «також у: …» у кожній з них — окрім себе самої.
    #[test]
    fn category_window_includes_also_in_from_multi_hit_categories() {
        use tauri::Manager;
        let (app, webview) = test_app();
        let scan = app.state::<crate::scan_runtime::ScanRuntime>();
        // 200 МіБ + давній accessed_at (sample_file_record) задовольняє
        // одразу large_files (≥100 МіБ) і old_files (≥365 дн) дефолтів.
        scan.index
            .insert_batch(vec![sample_file_record(
                10,
                200 * 1024 * 1024,
                CategoryId::Uncategorized,
                Decision::Undecided,
            )])
            .unwrap();
        // apply_settings прогонить реєстр детекторів і побудує also_in (T-121),
        // так само як реальний settings.set/scan.start.
        scan.apply_settings(&AppSettings::default())
            .expect("apply_settings");

        let response = get_ipc_response(
            &webview,
            request(
                "category_window",
                json!({ "payload": { "categoryId": "large_files" } }),
            ),
        )
        .expect("category.window");
        let list = body_json(response);
        let candidates = list.as_array().expect("масив кандидатів");
        assert_eq!(candidates.len(), 1);
        assert_eq!(
            candidates[0]["alsoIn"],
            json!(["old_files"]),
            "primary — large_files (перший у реєстрі); old_files лишається \
             маркером перетину, файл живе лише в сітці своєї primary-категорії"
        );
    }

    #[test]
    fn category_window_rejects_unknown_category() {
        let (_app, webview) = test_app();
        let result = get_ipc_response(
            &webview,
            request(
                "category_window",
                json!({ "payload": { "categoryId": "not_a_category" } }),
            ),
        );
        assert!(result.is_err());
    }

    /// DoD T-115: зміна порога перебудовує категорію з індексу без рескану.
    #[test]
    fn category_set_threshold_recalculates_index_in_place() {
        let (app, webview) = test_app();
        use tauri::Manager;
        let scan = app.state::<crate::scan_runtime::ScanRuntime>();
        scan.index
            .insert_batch(vec![sample_file_record(
                1,
                50 * 1024 * 1024,
                CategoryId::Uncategorized,
                Decision::Undecided,
            )])
            .unwrap();

        let response = get_ipc_response(
            &webview,
            request(
                "category_set_threshold",
                json!({ "payload": { "categoryId": "large_files", "key": "min_size_bytes", "value": 10 * 1024 * 1024 } }),
            ),
        )
        .expect("category.set_threshold");
        let settings = body_json(response);
        assert_eq!(
            settings["detectors"]["large_files"]["thresholds"]["min_size_bytes"],
            10 * 1024 * 1024
        );

        let records = scan.index.get_all();
        assert_eq!(records[0].category, CategoryId::LargeFiles);
    }

    #[test]
    fn category_set_threshold_rejects_category_without_thresholds() {
        let (_app, webview) = test_app();
        let result = get_ipc_response(
            &webview,
            request(
                "category_set_threshold",
                json!({ "payload": { "categoryId": "temp_files", "key": "min_size_bytes", "value": 1 } }),
            ),
        );
        assert!(
            result.is_err(),
            "temp_files не має редагованих порогів (реєстрова, не предикатна категорія)"
        );
    }

    /// DoD T-120: невідомий кандидат — типізована відмова, не паніка.
    #[test]
    fn preview_thumbnail_rejects_unknown_candidate() {
        let (_app, webview) = test_app();
        let result = get_ipc_response(
            &webview,
            request(
                "preview_thumbnail",
                json!({ "payload": { "candidateId": 999 } }),
            ),
        );
        assert!(result.is_err());
    }

    /// Кандидат без валідного кешу (перший запит) → "scheduled", ack
    /// повертається одразу — команда неблокуюча (architecture.md §1.2),
    /// незалежно від того, встигне фонова генерація завершитись чи ні
    /// (файл `C:\test\file-1.bin` у тесті не існує на диску).
    #[test]
    fn preview_thumbnail_schedules_generation_when_cache_miss() {
        use tauri::Manager;
        let (app, webview) = test_app();
        app.state::<crate::scan_runtime::ScanRuntime>()
            .index
            .insert_batch(vec![sample_file_record(
                1,
                1024,
                CategoryId::LargeFiles,
                Decision::Undecided,
            )])
            .unwrap();

        let response = get_ipc_response(
            &webview,
            request(
                "preview_thumbnail",
                json!({ "payload": { "candidateId": 1 } }),
            ),
        )
        .expect("preview.thumbnail");
        let ack = body_json(response);
        assert_eq!(ack["status"], "scheduled");
        assert_eq!(ack["dataUrl"], serde_json::Value::Null);
    }

    /// Папка-одиниця (T-053) — немає єдиного файла для мініатюри (DoD T-120).
    #[test]
    fn preview_thumbnail_unavailable_for_folder_unit() {
        use tauri::Manager;
        let (app, webview) = test_app();
        let mut folder_record =
            sample_file_record(2, 2048, CategoryId::DevArtifacts, Decision::Undecided);
        folder_record.unit = trashradar_domain::candidate::CandidateUnit::Folder;
        app.state::<crate::scan_runtime::ScanRuntime>()
            .index
            .insert_batch(vec![folder_record])
            .unwrap();

        let response = get_ipc_response(
            &webview,
            request(
                "preview_thumbnail",
                json!({ "payload": { "candidateId": 2 } }),
            ),
        )
        .expect("preview.thumbnail");
        let ack = body_json(response);
        assert_eq!(ack["status"], "unavailable");
    }

    /// Скраб для файла-фантома (без ffmpeg/недоступного шляху) деградує до
    /// порожньої смуги, а не помилки — UI лишається на статичній мініатюрі.
    #[test]
    fn preview_scrub_strip_degrades_to_empty_for_unavailable_video() {
        use tauri::Manager;
        let (app, webview) = test_app();
        app.state::<crate::scan_runtime::ScanRuntime>()
            .index
            .insert_batch(vec![sample_file_record(
                3,
                4096,
                CategoryId::ForgottenVideos,
                Decision::Undecided,
            )])
            .unwrap();

        let response = get_ipc_response(
            &webview,
            request(
                "preview_scrub_strip",
                json!({ "payload": { "candidateId": 3 } }),
            ),
        )
        .expect("preview.scrub_strip");
        let ack = body_json(response);
        assert_eq!(ack["frameCount"], 0);
        assert_eq!(ack["frames"], json!([]));
    }

    /// DoD T-124: невідомий кандидат — типізована відмова, не паніка.
    #[test]
    fn preview_large_rejects_unknown_candidate() {
        let (_app, webview) = test_app();
        let result = get_ipc_response(
            &webview,
            request(
                "preview_large",
                json!({ "payload": { "candidateId": 999 } }),
            ),
        );
        assert!(result.is_err());
    }

    /// Папка-одиниця (T-053) — немає єдиного файла для превью.
    #[test]
    fn preview_large_unavailable_for_folder_unit() {
        use tauri::Manager;
        let (app, webview) = test_app();
        let mut folder_record =
            sample_file_record(4, 2048, CategoryId::DevArtifacts, Decision::Undecided);
        folder_record.unit = trashradar_domain::candidate::CandidateUnit::Folder;
        app.state::<crate::scan_runtime::ScanRuntime>()
            .index
            .insert_batch(vec![folder_record])
            .unwrap();

        let response = get_ipc_response(
            &webview,
            request("preview_large", json!({ "payload": { "candidateId": 4 } })),
        )
        .expect("preview.large");
        let ack = body_json(response);
        assert_eq!(ack["status"], "unavailable");
        assert_eq!(ack["dataUrl"], serde_json::Value::Null);
    }

    /// Кандидат без валідного кешу (перший запит, файл `C:\test\...` не
    /// існує на диску в тесті) → жодної синхронної доставки, лише фонову
    /// P0-задачу заплановано — команда все одно повертається одразу
    /// (architecture.md §1.2, неблокуюча).
    #[test]
    fn preview_large_schedules_when_no_cache_available() {
        use tauri::Manager;
        let (app, webview) = test_app();
        app.state::<crate::scan_runtime::ScanRuntime>()
            .index
            .insert_batch(vec![sample_file_record(
                5,
                1024,
                CategoryId::LargeFiles,
                Decision::Undecided,
            )])
            .unwrap();

        let response = get_ipc_response(
            &webview,
            request("preview_large", json!({ "payload": { "candidateId": 5 } })),
        )
        .expect("preview.large");
        let ack = body_json(response);
        assert_eq!(ack["status"], "sharp_scheduled_only");
        assert_eq!(ack["quality"], serde_json::Value::Null);
        assert_eq!(ack["dataUrl"], serde_json::Value::Null);
    }

    /// DoD T-125: невідомий кандидат — типізована відмова, не паніка.
    /// Успішний шлях (реальний `explorer.exe /select,...`) свідомо НЕ
    /// тестується тут — він і справді відкрив би вікно Провідника під час
    /// `cargo test` (той самий принцип, що й T-034: реальний UAC-діалог не
    /// викликається в CI, лише branch, що його не відкриває).
    #[test]
    fn candidate_reveal_in_explorer_rejects_unknown_candidate() {
        let (_app, webview) = test_app();
        let result = get_ipc_response(
            &webview,
            request(
                "candidate_reveal_in_explorer",
                json!({ "payload": { "candidateId": 999 } }),
            ),
        );
        assert!(result.is_err());
    }
}
