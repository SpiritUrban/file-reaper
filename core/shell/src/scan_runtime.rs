//! Рантайм сесії скану в shell (T-033): IPC start/stop + прогрес-події.
//!
//! Бізнес-логіка сесії — `trashradar_app::scan_control`; тут лише
//! Tauri State, фоновий потік і адаптери MFT/walk.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Runtime, State};
use trashradar_app::detectors::{
    AppCachesDetector, BuildArtifactsDetector, DetectorHit, DetectorId, DetectorOrchestrator,
    DetectorRegistry, NodeModulesDetector, TempFilesDetector, ThresholdValue,
    UnityArtifactsDetector, DUPLICATES_CASCADE_DETECTOR_ID,
};
use trashradar_app::location_registry::KnownLocationsRegistry;
use trashradar_app::ports::{HotIndex, ScanEnvironment};
use trashradar_app::scan_control::{
    run_scan_session, CancellableVolumeScanner, ScanBatchMeta, ScanController, ScanProgress,
    ScanProgressPhase, VolumeScanOutcome,
};
use trashradar_app::scan_strategy::{choose_scan_strategy, VolumeCapabilities};
use trashradar_app::workers::CancellationToken;
use trashradar_app::{
    mark_confirmed_groups, mvp_predicate_registry, run_duplicate_cascade_with_cache, Aggregator,
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
use trashradar_scan_walk::{PathExclusions, WalkConfig, WalkPathResolver};

use crate::events;
use crate::ipc::{record_command, record_command_error};

static NEXT_SCAN_SESSION_ID: AtomicU64 = AtomicU64::new(1);

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

    /// Швидко підмінити settings без перерахунку індексу (UI toggle /
    /// excluded volumes діють одразу; важкий `apply_settings` — у фоні).
    pub fn replace_settings_snapshot(&self, settings: AppSettings) {
        *self.settings.write().expect("scan settings lock") = settings;
    }

    pub fn apply_settings(&self, settings: &AppSettings) -> Result<u64, CoreError> {
        let registry = configured_registry(settings)?;
        let stats = DetectorOrchestrator::new(&registry)
            .recalculate_index(self.index.as_ref(), &CancellationToken::new())?;
        *self.settings.write().expect("scan settings lock") = settings.clone();

        // T-121: перерахунок «також у» після зміни порогів — recalculate_index
        // уже прогнав ці ж hits і відкинув усі, крім primary; друга легка
        // вибірка (evaluate_record без запису в індекс) збирає повний набір
        // категорій на кандидата.
        //
        // T-152 fix: ферма не заявляє hits каскаду дублікатів — після clear
        // also_in і (коли primary farm-категорії знято) Uncategorized-учасники
        // груп відновлюються з кешованого `duplicates.groups`. Primary
        // `duplicates_cascade` також захищений у `recalculate_index`.
        {
            let mut also_in = self.also_in.lock().expect("also_in lock");
            also_in.clear();
            let records = self.index.get_all();
            for record in &records {
                if record.decision == Decision::Keep {
                    continue;
                }
                record_hits(&mut also_in, &registry.evaluate_record(record));
            }
            let groups = self
                .duplicates
                .lock()
                .expect("duplicates lock")
                .groups
                .clone();
            reapply_cached_duplicates(self.index.as_ref(), &mut also_in, &groups);
        }

        let records = self.index.get_all();
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

/// Відновити hits каскаду дублікатів після farm-recalculate (T-152).
///
/// Для кожного не-keep учасника кешованої групи: (1) `also_in` += Duplicates;
/// (2) якщо primary зараз `Uncategorized` (ферма зняла / ніколи не мала
/// категорії) — промотувати в primary Duplicates, як `run_duplicates_cascade`.
fn reapply_cached_duplicates(
    index: &InMemoryIndex,
    also_in: &mut HashMap<u64, CategoryMask>,
    groups: &[MarkedDuplicateGroup],
) {
    if groups.is_empty() {
        return;
    }
    let records = index.get_all();
    let by_id: HashMap<CandidateId, &FileRecord> =
        records.iter().map(|r| (r.candidate_id, r)).collect();
    let mut promoted = Vec::new();
    for group in groups {
        for member in &group.members {
            if member.keep {
                continue;
            }
            also_in
                .entry(member.candidate_id.0)
                .or_default()
                .insert(CategoryId::Duplicates);
            if let Some(rec) = by_id.get(&member.candidate_id) {
                if rec.category == CategoryId::Uncategorized {
                    let mut promoted_rec = (*rec).clone();
                    promoted_rec.category = CategoryId::Duplicates;
                    promoted_rec.safety = SafetyLevel::SafeToBulk;
                    promoted_rec.detector_id = DUPLICATES_CASCADE_DETECTOR_ID.into();
                    promoted_rec.explanation =
                        duplicate_explanation(group.size.0, group.members.len());
                    promoted.push(promoted_rec);
                }
            }
        }
    }
    if !promoted.is_empty() {
        if let Err(e) = index.upsert_batch(promoted) {
            tracing::warn!(
                error = %e,
                "T-152: не вдалося відновити категорію Дублікати після recalculate"
            );
        }
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
                        promoted_rec.detector_id = DUPLICATES_CASCADE_DETECTOR_ID.into();
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

/// Пост-скановий пас папок-одиниць: порожні та майже порожні папки (розділи
/// «Порожні папки» / «Майже порожні»). Той самий патерн, що й
/// [`run_duplicates_cascade`] — інʼєкція готової категорії поза фермою
/// детекторів: агрегуємо повний перелік директорій тому (винесений сканером у
/// `dir_sink`, порожні папки невидимі файловому конвеєру) + файли з індексу,
/// `upsert` folder-одиниць зі стабільним namespaced-id, оновлення live-цифри.
fn run_folder_units_pass<R: Runtime>(
    app: &AppHandle<R>,
    index: &InMemoryIndex,
    last_totals: &Mutex<LiveTotals>,
    dir_sink: &Mutex<Vec<String>>,
    sparse_max_files: u32,
    deep_path_max_depth: u32,
    dev_artifacts_enabled: bool,
    cancel: &CancellationToken,
) {
    if cancel.is_cancelled() {
        return;
    }
    let dir_paths = match dir_sink.lock() {
        Ok(mut guard) => std::mem::take(&mut *guard),
        Err(_) => return,
    };
    // Повні записи потрібні dev-детекторам (маркери package.json, дати
    // активності); empty/sparse/deep беруть з них лише (path, size) файлів.
    let all = index.get_all();

    // Кожну групу folder-одиниць застосовуємо й публікуємо ОКРЕМО — так лічильники
    // відповідних розділів у Sidebar «тікають» покроково під час пост-сканового
    // аналізу, а не зʼявляються всі разом наприкінці (прогрес по групах).
    let mut total_units = 0usize;
    let mut flush = |units: Vec<FileRecord>| {
        if units.is_empty() {
            return;
        }
        total_units += units.len();
        if let Ok(mut live) = last_totals.lock() {
            live.ingest_primary(&units);
            events::emit_cleanup_totals(app, &live.summary());
        }
        if let Err(error) = index.upsert_batch(units) {
            tracing::warn!(%error, "folder-units: не вдалося застосувати папки-одиниці в індекс");
        }
    };

    // Порожні / майже порожні / глибокі — з переліку директорій + файлів.
    if !dir_paths.is_empty() {
        let files: Vec<(String, u64)> = all
            .iter()
            .filter(|r| r.unit == CandidateUnit::File)
            .map(|r| (r.path.clone(), r.size.0))
            .collect();
        flush(trashradar_app::detectors::detect_folder_units(
            &dir_paths,
            &files,
            trashradar_app::detectors::FolderScanConfig {
                sparse_max_files,
                deep_path_max_depth,
            },
        ));
    }

    // Dev-сміття (T-049…T-053): структурні детектори збирають маркери зі шляхів
    // індексу, тоді згортають кожну виявлену теку в одну folder-одиницю
    // (окремий namespace id — без колізій з empty/sparse/deep). Це вмикає
    // розділ «Dev-сміття», який досі не був підключений до живого скану.
    // Кожен детектор публікується окремо — прогрес розділу «Dev-сміття» видно.
    if dev_artifacts_enabled {
        let path_refs: Vec<&str> = all.iter().map(|r| r.path.as_str()).collect();
        let node_modules = NodeModulesDetector::from_index_paths(path_refs.iter().copied());
        flush(node_modules.aggregate_units(&all));
        let build_artifacts = BuildArtifactsDetector::from_index_paths(path_refs.iter().copied());
        flush(build_artifacts.aggregate_units(&all));
        let unity = UnityArtifactsDetector::from_index_paths(path_refs.iter().copied());
        flush(unity.aggregate_units(&all));
    }

    tracing::info!(
        dirs = dir_paths.len(),
        units = total_units,
        "folder-units: порожні/майже порожні/глибокі + dev-сміття додано"
    );
}

fn duplicate_explanation(group_size_bytes: u64, member_count: usize) -> String {
    let gb = group_size_bytes as f64 / (1024.0 * 1024.0 * 1024.0);
    format!("дублікат · {member_count} копії по {gb:.1} ГБ")
}

fn configured_registry(settings: &AppSettings) -> Result<DetectorRegistry, CoreError> {
    let mut registry = mvp_predicate_registry();
    // Локаційні детектори (T-047): «Тимчасові» + «Кеші» з реєстру відомих
    // локацій (registry/known-locations.json). Best-effort: якщо реєстр не
    // знайдено (деплой без теки registry/), ці розділи лишаються порожні, але
    // скан не падає.
    match KnownLocationsRegistry::load_default_or_embedded() {
        Ok(locations) => {
            registry.register(TempFilesDetector::from_registry(&locations));
            registry.register(AppCachesDetector::from_registry(&locations));
            tracing::info!(
                locations = locations.len(),
                "known-locations підключено — розділи Тимчасові/Кеші активні"
            );
        }
        Err(error) => {
            tracing::warn!(%error, "known-locations не завантажено (ні файл, ні вбудований) — Тимчасові/Кеші будуть порожні");
        }
    }
    for (id, detector) in &settings.detectors {
        let detector_id = match id.as_str() {
            "large_files" => DetectorId::new("large_files"),
            "old_files" => DetectorId::new("old_files"),
            "forgotten_videos" => DetectorId::new("forgotten_videos"),
            "archives" => DetectorId::new("archives"),
            "installers" => DetectorId::new("installers"),
            "temp_files" => DetectorId::new("temp_files"),
            "app_caches" => DetectorId::new("app_caches"),
            // Не-farm перемикачі (каскад дублікатів, обчислювані folder-категорії
            // empty/sparse/deep) не мають детектора у фермі — їхній enabled
            // читається окремо (напр. `duplicates_enabled`), тут просто
            // пропускаємо, а не валимо весь settings.set помилкою.
            _ => continue,
        };
        // Детектор може бути не зареєстрований (напр. temp/caches без реєстру) —
        // тоді set_threshold/set_enabled поверне помилку; ігноруємо, щоб toggle
        // неіснуючого детектора не валив settings.set.
        if registry.get(detector_id).is_none() {
            continue;
        }
        for (key, value) in &detector.thresholds {
            registry.set_threshold(detector_id, key, ThresholdValue::U64(*value))?;
        }
        // T-152: вимкнений детектор не отримує потік (T-037); перерахунок
        // індексу після зміни — apply_settings.
        registry.set_enabled(detector_id, detector.enabled)?;
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
    /// Усього файлів у сесії (усі томи + поточний).
    pub files_indexed: u64,
    /// Файлів на поточному томі.
    pub volume_files_indexed: u64,
    /// Відомий total на поточному томі (`null` = ще невідомо / MFT стрім).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub volume_files_total: Option<u64>,
    /// MFT-записи / walk-об'єкти поточної підфази (росте навіть коли files=0).
    #[serde(default)]
    pub stage_units_done: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stage_units_total: Option<u64>,
    /// `"mft_dirs"` | `"mft_files"` | `"walking"` | `"indexing"` | `""`.
    #[serde(default)]
    pub stage: String,
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
            volume_files_indexed: p.volume_files_indexed,
            volume_files_total: p.volume_files_total,
            stage_units_done: p.stage_units_done,
            stage_units_total: p.stage_units_total,
            stage: p.stage.to_string(),
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

/// Набір виключених з аналізу літер томів (Налаштування → «Диски»).
fn excluded_volume_set(excluded: &[String]) -> std::collections::HashSet<char> {
    excluded.iter().filter_map(|s| parse_volume_letter(s)).collect()
}

fn resolve_volumes(
    payload: &ScanStartPayload,
    excluded: &[String],
) -> Result<Vec<char>, CoreError> {
    let excluded_set = excluded_volume_set(excluded);
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
            // Явно переданий, але виключений у Налаштуваннях том — не скануємо.
            if excluded_set.contains(&v) || out.contains(&v) {
                continue;
            }
            out.push(v);
        }
        if out.is_empty() {
            return Err(CoreError::invalid_argument(
                "Усі передані томи виключені з аналізу в Налаштуваннях.",
            ));
        }
        return Ok(out);
    }
    let env = WinScanEnvironment;
    let vols: Vec<char> = env
        .list_scan_volumes()
        .into_iter()
        .filter(|v| !excluded_set.contains(v))
        .collect();
    if vols.is_empty() {
        return Err(CoreError::new(
            ErrorCode::Io,
            "Немає томів для сканування (усі доступні виключені в Налаштуваннях).".to_string(),
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

/// Диспетчер: MFT або walk за T-028 на кожному томі. Несе користувацькі
/// виключені шляхи з settings сесії скану (T-151) — зливаються з системними
/// дефолтами T-027 на кожен том.
struct AutoVolumeScanner {
    user_excluded: Vec<String>,
    /// Повні шляхи всіх директорій усіх томів сесії (incl. порожніх), уже
    /// відфільтровані виключеннями — вхід пост-сканового folder-units пасу
    /// (розділи «Порожні/майже порожні папки»).
    dir_sink: Arc<Mutex<Vec<String>>>,
}

impl CancellableVolumeScanner for AutoVolumeScanner {
    fn scan_volume(
        &self,
        volume: char,
        cancel: &CancellationToken,
        on_batch: &mut dyn FnMut(Vec<FileRecord>, ScanBatchMeta) -> Result<(), CoreError>,
    ) -> Result<VolumeScanOutcome, CoreError> {
        scan_one_volume_auto(volume, &self.user_excluded, &self.dir_sink, cancel, on_batch)
    }
}

/// Додати директорію до стоку, якщо вона не під виключеннями (MFT перелічує
/// весь том — системні/карантинні/користувацькі теки треба відсіяти тут).
fn collect_dir(dir_sink: &Mutex<Vec<String>>, exclusions: &PathExclusions, path: String) {
    if exclusions.is_excluded(std::path::Path::new(&path)) {
        return;
    }
    if let Ok(mut sink) = dir_sink.lock() {
        sink.push(path);
    }
}

/// Повний набір виключень тому: системні дефолти T-027 + службовий каталог
/// карантину (T-088, architecture.md §7.4) + користувацькі шляхи з
/// Налаштувань (T-151). Користувацькі шляхи можуть указувати на будь-який
/// том — нормалізація префіксів робить чужі невлучними без окремого фільтра.
fn exclusions_for_volume(volume: char, user_excluded: &[String]) -> PathExclusions {
    let mut exclusions = PathExclusions::windows_system_defaults(volume);
    if volume.is_ascii_alphabetic() {
        exclusions.extend([format!(
            r"{}:\{}",
            volume.to_ascii_uppercase(),
            trashradar_domain::quarantine::SERVICE_DIRECTORY_NAME
        )]);
    }
    exclusions.extend(user_excluded);
    exclusions
}

/// Викидає з батча записи під виключеними шляхами (шлях MFT-скану: сирий
/// перелік усього тому фільтрується на рівні записів — піддерева там
/// відсікати нема де, на відміну від обходу T-027).
fn retain_not_excluded(batch: &mut Vec<FileRecord>, exclusions: &PathExclusions) {
    if exclusions.is_empty() {
        return;
    }
    batch.retain(|record| !exclusions.is_excluded(std::path::Path::new(&record.path)));
}

fn scan_one_volume_auto(
    volume: char,
    user_excluded: &[String],
    dir_sink: &Mutex<Vec<String>>,
    cancel: &CancellationToken,
    on_batch: &mut dyn FnMut(Vec<FileRecord>, ScanBatchMeta) -> Result<(), CoreError>,
) -> Result<VolumeScanOutcome, CoreError> {
    let env = WinScanEnvironment;
    let strategy = choose_scan_strategy(&VolumeCapabilities {
        is_ntfs: env.is_ntfs(volume).unwrap_or(false),
        is_elevated: env.is_elevated(),
    })
    .strategy;
    let exclusions = exclusions_for_volume(volume, user_excluded);
    tracing::info!(target: "scan_diag", volume = %volume, ?strategy, "strategy selected");
    match strategy {
        ScanStrategy::Mft => scan_mft_cancellable(volume, &exclusions, dir_sink, cancel, on_batch),
        ScanStrategy::DirectoryWalk | ScanStrategy::UsnDelta => {
            scan_walk_cancellable(volume, exclusions, dir_sink, cancel, on_batch)
        }
    }
}

fn scan_mft_cancellable(
    volume: char,
    exclusions: &PathExclusions,
    dir_sink: &Mutex<Vec<String>>,
    cancel: &CancellationToken,
    on_batch: &mut dyn FnMut(Vec<FileRecord>, ScanBatchMeta) -> Result<(), CoreError>,
) -> Result<VolumeScanOutcome, CoreError> {
    #[cfg(windows)]
    {
        use std::cell::{Cell, RefCell};
        // Менший батч → частіші тіки UI (було 10k — довгі паузи на 0).
        const BATCH: usize = 2_000;
        let files_kept = Cell::new(0u64);
        // Throttle progress-only тіків (pass1): ~5/с.
        let last_tick = Cell::new(
            std::time::Instant::now()
                .checked_sub(std::time::Duration::from_secs(1))
                .unwrap_or_else(std::time::Instant::now),
        );
        // Два callback-и (progress + sink) ділять один on_batch.
        let batch_cb = RefCell::new(on_batch);
        let (_stats, cancelled) =
            trashradar_scan_mft::pipeline::scan_volume_to_index_cancel_progress(
                volume,
                BATCH,
                || cancel.is_cancelled(),
                |p| {
                    let now = std::time::Instant::now();
                    let due = now.duration_since(last_tick.get())
                        >= std::time::Duration::from_millis(200)
                        || p.records_scanned >= p.records_total;
                    if !due {
                        return;
                    }
                    last_tick.set(now);
                    let stage = if p.pass == 1 { "mft_dirs" } else { "mft_files" };
                    let _ = (batch_cb.borrow_mut())(
                        Vec::new(),
                        ScanBatchMeta {
                            volume_files_so_far: p.files_emitted.max(files_kept.get()),
                            volume_files_total: None,
                            units_done: p.records_scanned,
                            units_total: Some(p.records_total.max(1)),
                            stage,
                        },
                    );
                },
                |dir_path: String| collect_dir(dir_sink, exclusions, dir_path),
                |mut batch: Vec<FileRecord>| {
                    retain_not_excluded(&mut batch, exclusions);
                    if batch.is_empty() {
                        return Ok(());
                    }
                    let n = files_kept.get() + batch.len() as u64;
                    files_kept.set(n);
                    (batch_cb.borrow_mut())(
                        batch,
                        ScanBatchMeta {
                            volume_files_so_far: n,
                            volume_files_total: None,
                            units_done: 0,
                            units_total: None,
                            stage: "indexing",
                        },
                    )
                },
            )?;
        Ok(VolumeScanOutcome {
            files_indexed: files_kept.get(),
            cancelled,
        })
    }
    #[cfg(not(windows))]
    {
        let _ = (volume, exclusions, cancel, on_batch);
        Err(CoreError::not_implemented("scan.mft"))
    }
}

fn scan_walk_cancellable(
    volume: char,
    exclusions: PathExclusions,
    dir_sink: &Mutex<Vec<String>>,
    cancel: &CancellationToken,
    on_batch: &mut dyn FnMut(Vec<FileRecord>, ScanBatchMeta) -> Result<(), CoreError>,
) -> Result<VolumeScanOutcome, CoreError> {
    if cancel.is_cancelled() {
        return Ok(VolumeScanOutcome {
            files_indexed: 0,
            cancelled: true,
        });
    }

    // T-027/T-151: у виключені піддерева обхід не заходить взагалі.
    // Клон у config — сам `exclusions` лишаємо для фільтра директорій нижче.
    let config = WalkConfig {
        exclusions: exclusions.clone(),
        ..WalkConfig::default()
    };

    // Живий тік під час enumeration (walk збирає все, потім віддає) —
    // інакше UI стоїть на 0, поки walk_volume не завершиться.
    let _ = on_batch(
        Vec::new(),
        ScanBatchMeta {
            volume_files_so_far: 0,
            volume_files_total: None,
            units_done: 0,
            units_total: None,
            stage: "walking",
        },
    );

    let walk_started = Instant::now();
    tracing::info!(target: "scan_diag", volume = %volume, "directory walk started");
    let all = trashradar_scan_walk::walk_volume(volume, config)?;
    tracing::info!(target: "scan_diag", volume = %volume, entries = all.len(), elapsed_ms = walk_started.elapsed().as_millis(), cancelled = cancel.is_cancelled(), "directory walk finished");
    if cancel.is_cancelled() {
        return Ok(VolumeScanOutcome {
            files_indexed: 0,
            cancelled: true,
        });
    }

    let volume_files_total = all.iter().filter(|e| !e.is_directory).count() as u64;
    tracing::info!(target: "scan_diag", volume = %volume, entries = all.len(), files = volume_files_total, "walk results ready for indexing");
    // Total відомий — одразу показуємо 0 / N перед індексацією.
    let _ = on_batch(
        Vec::new(),
        ScanBatchMeta {
            volume_files_so_far: 0,
            volume_files_total: Some(volume_files_total),
            units_done: all.len() as u64,
            units_total: Some(all.len() as u64),
            stage: "indexing",
        },
    );

    const BATCH: usize = 1_000;
    let mut files_indexed = 0u64;
    let mut batch = Vec::new();
    let mut next_id = 0u64;
    let resolver = WalkPathResolver::from_entries(volume, &all);

    let mut deliver = |batch: Vec<FileRecord>, files_indexed: u64| -> Result<(), CoreError> {
        on_batch(
            batch,
            ScanBatchMeta {
                volume_files_so_far: files_indexed,
                volume_files_total: Some(volume_files_total),
                units_done: files_indexed,
                units_total: Some(volume_files_total),
                stage: "indexing",
            },
        )
    };

    for e in &all {
        if cancel.is_cancelled() {
            if !batch.is_empty() {
                files_indexed += batch.len() as u64;
                deliver(std::mem::take(&mut batch), files_indexed)?;
            }
            return Ok(VolumeScanOutcome {
                files_indexed,
                cancelled: true,
            });
        }
        if e.is_directory {
            // Директорію (incl. порожню) виносимо в сток для folder-units пасу.
            // Обхід T-027 уже не заходить у виключені піддерева, але фільтруємо
            // ще раз для симетрії з MFT-шляхом.
            if let Some(dir_path) = resolver.full_path(e) {
                collect_dir(dir_sink, &exclusions, dir_path);
            }
            continue;
        }
        let Some(path) = resolver.full_path(e) else {
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
            deliver(std::mem::take(&mut batch), files_indexed)?;
        }
    }
    if !batch.is_empty() {
        files_indexed += batch.len() as u64;
        deliver(batch, files_indexed)?;
    }
    Ok(VolumeScanOutcome {
        files_indexed,
        cancelled: false,
    })
}

/// Ім'я dump-файла в корені репозиторію (завжди перезаписується).
const SCAN_FOUND_DUMP_NAME: &str = "scan-found-files.txt";

/// Корінь репо: `core/shell` → `../..`, або parent від cwd=`core` (tauri.mjs).
fn resolve_repo_root() -> PathBuf {
    let from_manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .ok();
    if let Some(p) = from_manifest {
        return p;
    }
    if let Ok(cwd) = std::env::current_dir() {
        if cwd.file_name().and_then(|s| s.to_str()) == Some("core") {
            if let Some(parent) = cwd.parent() {
                return parent.to_path_buf();
            }
        }
        return cwd;
    }
    PathBuf::from(".")
}

fn category_label(c: CategoryId) -> &'static str {
    match c {
        CategoryId::LargeFiles => "large_files",
        CategoryId::OldFiles => "old_files",
        CategoryId::ForgottenVideos => "forgotten_videos",
        CategoryId::Duplicates => "duplicates",
        CategoryId::Archives => "archives",
        CategoryId::Installers => "installers",
        CategoryId::TempFiles => "temp_files",
        CategoryId::AppCaches => "app_caches",
        CategoryId::DevArtifacts => "dev_artifacts",
        CategoryId::EmptyFolders => "empty_folders",
        CategoryId::SparseFolders => "sparse_folders",
        CategoryId::DeepPaths => "deep_paths",
        CategoryId::Uncategorized => "uncategorized",
    }
}

fn kind_label(k: FileKind) -> &'static str {
    match k {
        FileKind::Video => "video",
        FileKind::Image => "image",
        FileKind::Audio => "audio",
        FileKind::Archive => "archive",
        FileKind::Installer => "installer",
        FileKind::DiskImage => "disk_image",
        FileKind::Document => "document",
        FileKind::Other => "other",
    }
}

fn unit_label(u: CandidateUnit) -> &'static str {
    match u {
        CandidateUnit::File => "file",
        CandidateUnit::Folder => "folder",
    }
}

fn unix_now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Куди писати dump (усі шляхи перезаписуються одним вмістом).
fn scan_found_dump_paths() -> Vec<PathBuf> {
    let mut paths = vec![resolve_repo_root().join(SCAN_FOUND_DUMP_NAME)];
    if let Ok(local) = std::env::var("LOCALAPPDATA") {
        paths.push(
            PathBuf::from(local)
                .join("TrashRadar")
                .join(SCAN_FOUND_DUMP_NAME),
        );
    }
    // Дубль поруч із exe (core/target/debug) — якщо корінь репо недоступний.
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            paths.push(dir.join(SCAN_FOUND_DUMP_NAME));
        }
    }
    paths
}

/// Перезаписати `scan-found-files.txt`: усі записи індексу (size ↓).
/// Пише в кілька місць (корінь репо + LocalAppData + dir exe).
fn dump_scan_found_files(index: &InMemoryIndex, stage: &str) {
    let mut records = index.get_all();
    records.sort_by(|a, b| b.size.0.cmp(&a.size.0).then_with(|| a.path.cmp(&b.path)));
    let body = format_scan_found_dump(&records);
    for path in scan_found_dump_paths() {
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        match fs::write(&path, &body) {
            Ok(()) => tracing::info!(
                stage,
                path = %path.display(),
                bytes = body.len(),
                records = records.len(),
                "dump знайдених файлів перезаписано"
            ),
            Err(error) => tracing::warn!(
                stage,
                %error,
                path = %path.display(),
                "не вдалося записати dump знайдених файлів"
            ),
        }
    }
}

fn format_scan_found_dump(records: &[FileRecord]) -> String {
    let candidates = records
        .iter()
        .filter(|r| r.category != CategoryId::Uncategorized)
        .count();
    let mut out = String::with_capacity(records.len().saturating_mul(160).saturating_add(512));
    out.push_str("# TrashRadar scan-found-files — overwritten each scan pass\n");
    out.push_str(&format!("# generated_unix: {}\n", unix_now_secs()));
    out.push_str(&format!("# total_index: {}\n", records.len()));
    out.push_str(&format!("# candidates: {candidates}\n"));
    out.push_str(
        "# columns: size_bytes\tcategory\tkind\tunit\tsystem\thidden\tpath\texplanation\n",
    );
    for r in records {
        let explanation = r
            .explanation
            .replace(['\t', '\r', '\n'], " ")
            .trim()
            .to_string();
        out.push_str(&format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
            r.size.0,
            category_label(r.category),
            kind_label(r.kind),
            unit_label(r.unit),
            if r.attributes.is_system { 1 } else { 0 },
            if r.attributes.is_hidden { 1 } else { 0 },
            r.path,
            explanation
        ));
    }
    out
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
    // Виключені з аналізу томи (Налаштування → «Диски»). Читаємо з тієї самої
    // копії settings, що її оновлює settings.set через scan.apply_settings.
    let excluded_volumes = state
        .settings
        .read()
        .map(|s| s.scan.excluded_volumes.clone())
        .unwrap_or_default();
    let volumes = match resolve_volumes(&payload, &excluded_volumes) {
        Ok(v) => v,
        Err(e) => {
            record_command_error();
            return Err(e);
        }
    };
    let strategies = strategies_for(&volumes);
    let volume_count = volumes.len() as u32;
    let session_id = NEXT_SCAN_SESSION_ID.fetch_add(1, Ordering::Relaxed);
    tracing::info!(target: "scan_diag", session_id, ?volumes, ?strategies, "scan request accepted");

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
            let session_started = Instant::now();
            tracing::info!(target: "scan_diag", session_id, "scan worker started");
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

            // T-151: виключені шляхи фіксуються на старті сесії — зміна
            // списку в Налаштуваннях діє з наступного скану.
            // Сток директорій усіх томів (incl. порожніх) для folder-units пасу.
            let dir_paths: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
            let sparse_max_files = settings.scan.sparse_max_files;
            let deep_path_max_depth = settings.scan.deep_path_max_depth;
            // T-152-стиль перемикач каскаду дублікатів: галочка «Дублікати» в
            // Налаштуваннях. `enabled == false` → важкий каскад не запускаємо
            // (проблемний розділ, над яким працюємо окремо).
            let duplicates_enabled = settings
                .detectors
                .get("duplicates")
                .map(|d| d.enabled)
                .unwrap_or(true);
            let dev_artifacts_enabled = settings
                .detectors
                .get("dev_artifacts")
                .map(|d| d.enabled)
                .unwrap_or(true);
            let scanner = AutoVolumeScanner {
                user_excluded: settings.scan.excluded_paths.clone(),
                dir_sink: Arc::clone(&dir_paths),
            };
            // Глобально-унікальний candidate_id на всю сесію: per-volume сканери
            // (scan-mft `Batcher`, walk) видають id з 0 у КОЖНОМУ томі — тож без
            // цього зсуву id колізіонують між томами (C:/D:/…), і `find_record`
            // повертає файл не того тому → превʼю/«відкрити»/reap б'ють по чужому
            // файлу. Централізуємо тут, у єдиному стоці: працює для будь-якої
            // стратегії (MFT / walk / USN-дельта).
            let mut next_global_id: u64 = 0;
            let result = run_scan_session(
                &volumes,
                &strategies,
                &scanner,
                &token,
                |p| {
                    tracing::info!(target: "scan_diag", session_id, volume = %p.volume, phase = ?p.phase, stage = p.stage, files_indexed = p.files_indexed, volume_files_indexed = p.volume_files_indexed, volume_files_total = ?p.volume_files_total, stage_units_done = p.stage_units_done, stage_units_total = ?p.stage_units_total, volume_index = p.volume_index, volume_count = p.volume_count, done = p.done, cancelled = p.cancelled, elapsed_ms = session_started.elapsed().as_millis(), "scan progress emitted");
                    emit_progress(&app2, &p);
                },
                |vol, mut batch| {
                    let batch_started = Instant::now();
                    let batch_len = batch.len();
                    // Перепризначаємо id ПЕРЕД індексом і детекторами, щоб усе
                    // нижче (hits, also_in, live totals, surrogate-імена reap)
                    // бачило той самий унікальний id.
                    for record in &mut batch {
                        record.candidate_id = CandidateId(next_global_id);
                        next_global_id += 1;
                    }
                    let index_before = HotIndex::len(index.as_ref()).unwrap_or(0);
                    // 1) сирі записи в індекс
                    index.insert_batch(batch.clone())?;
                    // 2) детектори → primary + hits
                    let cat = orch.categorize_batch(&batch);
                    let categorized = cat.updated.len();
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
                    tracing::info!(target: "scan_diag", session_id, volume = %vol, batch_files = batch_len, index_before, index_after = HotIndex::len(index.as_ref()).unwrap_or(0), categorized, detector_hits = cat.hits.len(), batch_elapsed_ms = batch_started.elapsed().as_millis(), session_elapsed_ms = session_started.elapsed().as_millis(), "scan batch committed");
                    Ok(())
                },
            );
            match &result {
                Ok(summary) => tracing::info!(target: "scan_diag", session_id, files_indexed = summary.files_indexed, volumes_completed = summary.volumes_completed, cancelled = summary.cancelled, index_len = HotIndex::len(index.as_ref()).unwrap_or(0), elapsed_ms = session_started.elapsed().as_millis(), "scan pipeline finished"),
                Err(error) => tracing::error!(target: "scan_diag", session_id, %error, index_len = HotIndex::len(index.as_ref()).unwrap_or(0), elapsed_ms = session_started.elapsed().as_millis(), "scan pipeline failed"),
            }
            // Dump ОДРАЗУ після індексації томів — до каскаду дублікатів
            // (каскад може бути довгим; dump потрібен для аналізу keep-hints).
            dump_scan_found_files(index.as_ref(), "after_pipeline");
            // T-126: каскад дублікатів — UI лишається «зайнятим» (done=false),
            // щоб не виглядало, ніби все вже готово, поки йде хешування.
            let (files_indexed, session_cancelled) = match &result {
                Ok(summary) => (summary.files_indexed, summary.cancelled),
                Err(_) => (0, token.is_cancelled()),
            };
            if !session_cancelled {
                if duplicates_enabled {
                    emit_progress(
                        &app2,
                        &ScanProgress {
                            volume: volumes.first().copied().unwrap_or('?'),
                            strategy: strategies
                                .first()
                                .copied()
                                .unwrap_or(ScanStrategy::DirectoryWalk),
                            phase: ScanProgressPhase::DuplicatesCascade,
                            files_indexed,
                            volume_files_indexed: files_indexed,
                            volume_files_total: Some(files_indexed),
                            stage_units_done: 0,
                            stage_units_total: None,
                            stage: "cascade",
                            volume_index: volume_count.saturating_sub(1),
                            volume_count,
                            done: false,
                            cancelled: false,
                        },
                    );
                    run_duplicates_cascade(
                        &app2,
                        index.as_ref(),
                        &last_totals,
                        &also_in,
                        &duplicates,
                        &hash_cache,
                        &token,
                    );
                } else {
                    tracing::info!(session_id, "каскад дублікатів вимкнено в Налаштуваннях — пропущено");
                }
                // Порожні/майже порожні папки — після каскаду (спільний індекс,
                // окремий namespace id, без колізій з файлами/дублікатами).
                run_folder_units_pass(
                    &app2,
                    index.as_ref(),
                    &last_totals,
                    &dir_paths,
                    sparse_max_files,
                    deep_path_max_depth,
                    dev_artifacts_enabled,
                    &token,
                );
            }

            // Повторний dump після folder-units / cascade (повніший набір).
            dump_scan_found_files(index.as_ref(), "after_postprocess");

            // Фінальний snapshot без втрати підсумку (T-055 / T-006 flush).
            let _ = throttle.flush(Instant::now());
            if let Ok(live) = last_totals.lock() {
                let summary = live.summary();
                events::emit_cleanup_totals(&app2, &summary);
                // T-055 діагностика: live-лічильники (мульти-hit, інкрементні)
                // звіряються з перерахунком primary-індексу. Розбіжність — це
                // сигнал телеметрії, а НЕ причина рвати scan-потік: колишній
                // debug_assert! панікував тут і UI ніколи не отримував
                // SessionComplete нижче (лишався «зайнятим» назавжди). У release
                // debug_assert і так компілювався геть — тобто в проді розбіжність
                // уже була нефатальною; тут робимо dev таким же стійким, лишаючи
                // гучний ERROR із дельтою для діагностики за реальними даними.
                // InMemoryIndex::get_all — inherent Vec; trait HotIndex returns Result.
                let index_summary = Aggregator::from_primary_records(&index.get_all());
                if summary.unique_bytes != index_summary.unique_bytes
                    || summary.unique_files != index_summary.unique_files
                {
                    tracing::error!(
                        live_unique_files = summary.unique_files,
                        index_unique_files = index_summary.unique_files,
                        live_unique_bytes = summary.unique_bytes.0,
                        index_unique_bytes = index_summary.unique_bytes.0,
                        delta_files = summary.unique_files as i64 - index_summary.unique_files as i64,
                        delta_bytes = summary.unique_bytes.0 as i64 - index_summary.unique_bytes.0 as i64,
                        "T-055: live-тотали розійшлися з індексом (не фатально; індекс — джерело істини)"
                    );
                } else {
                    tracing::info!(
                        reclaimable = summary.unique_bytes.0,
                        unique_files = summary.unique_files,
                        "live totals після скану збігаються з індексом"
                    );
                }
            }
            // Закриття busy-стану UI (після томів + каскаду).
            emit_progress(
                &app2,
                &ScanProgress {
                    volume: volumes.first().copied().unwrap_or('?'),
                    strategy: strategies
                        .first()
                        .copied()
                        .unwrap_or(ScanStrategy::DirectoryWalk),
                    phase: ScanProgressPhase::SessionComplete,
                    files_indexed,
                    volume_files_indexed: files_indexed,
                    volume_files_total: Some(files_indexed),
                    stage_units_done: 0,
                    stage_units_total: None,
                    stage: "",
                    volume_index: volume_count.saturating_sub(1),
                    volume_count,
                    done: true,
                    cancelled: session_cancelled || token.is_cancelled(),
                },
            );
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
    // Не get_all() на 100k+ записів — лише потрібні id (лаг UI при reap).
    let records: Vec<FileRecord> = state.index.get_by_candidate_ids(&wanted);
    if records.is_empty() {
        record_command_error();
        return Err(CoreError::invalid_argument(
            "Жоден з candidateIds не знайдено в живому індексі.",
        ));
    }

    let profile_dir = profile.profile_dir();
    let profile_dir_for_badge = profile_dir.clone();
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

    // Живий бейдж + рефетч екрана Quarantine (раніше подія не летіла → UI «пусто»).
    let badge = tauri::async_runtime::spawn_blocking(move || {
        crate::ipc::read_profile_quarantine_badge(profile_dir_for_badge)
    })
    .await
    .unwrap_or_else(|_| crate::ipc::QuarantineBadge::default());
    events::emit_quarantine_changed(&app, &badge, 0, 0, false, None);

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

    let roots = vec![profile_root];
    let reaper = trashradar_app::TransactionalReaper::new(&filesystem, &database, &roots);

    // Per-item, стійко до збоїв: один проблемний кандидат (недоступна тека,
    // хмарний плейсхолдер, заблокований файл, змінений файл) НЕ валить увесь
    // reap — його пропускаємо з warn-логом, решту переміщуємо. Це критично для
    // папок-одиниць (T-053): серед сотень тек майже завжди трапляється одна
    // «важка», а стара батч-семантика через `?`/`reap_batch` через це давала
    // «затримка й нічого». Помилку повертаємо лише якщо НЕ переміщено жодного.
    let mut outcomes: Vec<trashradar_app::ReapOutcome> = Vec::with_capacity(records.len());
    let mut first_error: Option<CoreError> = None;
    let mut skipped = 0u64;

    for record in &records {
        let Some(volume) = volume_of(&record.path) else {
            skipped += 1;
            continue;
        };
        // Жива ідентичність (T-086): для файла — ще й пре-перевірка «не
        // змінився» (розмір + mtime до секунди, як зберіг гарячий індекс).
        // Для папки-одиниці метадані директорії дають size ≈ 0 ≠ сумі
        // піддерева, тож пре-перевірку пропускаємо (цілісність move гарантує
        // `validate_file_identity` усередині move_into_quarantine).
        let live = match trashradar_platform_win::read_file_identity(std::path::Path::new(
            &record.path,
        )) {
            Ok(live) => live,
            Err(error) => {
                tracing::warn!(path = %record.path, %error, "reap: не вдалось прочитати ідентичність — пропущено");
                first_error.get_or_insert(error);
                skipped += 1;
                continue;
            }
        };
        if record.unit == CandidateUnit::File {
            let record_seconds = record.modified_at.map(|timestamp| timestamp.0 / 10_000_000);
            let live_seconds = live.modified_at.map(|timestamp| timestamp.0 / 10_000_000);
            if live.size != record.size || live_seconds != record_seconds {
                tracing::warn!(path = %record.path, "reap: файл змінився після сканування — пропущено");
                first_error.get_or_insert(CoreError::file_changed(format!(
                    "Файл «{}» змінився після сканування; пропущено.",
                    record.path
                )));
                skipped += 1;
                continue;
            }
        }
        let directory = match filesystem.ensure_on_volume(volume) {
            Ok(directory) => directory,
            Err(error) => {
                tracing::warn!(volume = %volume, %error, "reap: том без каталогу карантину — пропущено");
                first_error.get_or_insert(error);
                skipped += 1;
                continue;
            }
        };
        let surrogate_name = format!("{:016}.bin", record.candidate_id.0);
        let destination = directory.quarantine_root.join(&surrogate_name);
        let request = trashradar_app::ReapRequest {
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
        };
        match reaper.reap_one(request) {
            Ok(outcome) => outcomes.push(outcome),
            Err(error) => {
                tracing::warn!(path = %record.path, unit = ?record.unit, %error, "reap: переміщення кандидата провалилось — пропущено");
                first_error.get_or_insert(error);
                skipped += 1;
            }
        }
    }

    if outcomes.is_empty() {
        return Err(first_error.unwrap_or_else(|| {
            CoreError::invalid_argument("Жоден позначений кандидат не резолвився на локальний том.")
        }));
    }
    if skipped > 0 {
        tracing::warn!(
            reaped = outcomes.len(),
            skipped,
            "reap: частину кандидатів пропущено (деталі — warn вище)"
        );
    }
    Ok(outcomes)
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
    let profile_dir_for_badge = profile_dir.clone();
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
    let badge = tauri::async_runtime::spawn_blocking(move || {
        crate::ipc::read_profile_quarantine_badge(profile_dir_for_badge)
    })
    .await
    .unwrap_or_else(|_| crate::ipc::QuarantineBadge::default());
    events::emit_quarantine_changed(&app, &badge, 0, 0, false, None);
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
        let v = resolve_volumes(&p, &[]).unwrap();
        assert_eq!(v, vec!['C', 'D']);
    }

    #[test]
    fn resolve_volumes_drops_excluded_disks() {
        // Явний список: виключений том відсіюється; лишається решта.
        let p = ScanStartPayload {
            volumes: Some(vec!["C".into(), "D".into(), "E".into()]),
        };
        let v = resolve_volumes(&p, &["D".into()]).unwrap();
        assert_eq!(v, vec!['C', 'E']);

        // Усі передані виключені → помилка (немає що сканувати).
        let all_excluded = resolve_volumes(&p, &["C".into(), "D".into(), "E".into()]);
        assert!(all_excluded.is_err());
    }

    /// T-151: набір виключень тому = системні дефолти T-027 + `.trashradar`
    /// (T-088) + користувацькі шляхи з Налаштувань.
    #[test]
    fn volume_exclusions_merge_system_service_and_user_paths() {
        let user = vec![r"C:\Users\Ada\OneDrive".to_string()];
        let ex = exclusions_for_volume('C', &user);
        assert!(ex.is_excluded(std::path::Path::new(r"C:\Windows\System32")));
        assert!(ex.is_excluded(std::path::Path::new(r"C:\.trashradar\quarantine\x.bin")));
        assert!(ex.is_excluded(std::path::Path::new(r"c:\users\ada\onedrive\video.mp4")));
        assert!(!ex.is_excluded(std::path::Path::new(r"C:\Users\Ada\Downloads\video.mp4")));
    }

    /// T-151 (шлях MFT): записи під виключеним шляхом викидаються з батча,
    /// решта лишається незмінною.
    #[test]
    fn retain_not_excluded_drops_only_records_under_excluded_prefix() {
        fn record(id: u64, path: &str) -> FileRecord {
            FileRecord {
                candidate_id: CandidateId(id),
                path: path.into(),
                size: ByteSize(1),
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
            }
        }
        let ex = exclusions_for_volume('C', &[r"C:\Skip".to_string()]);
        let mut batch = vec![
            record(1, r"C:\Skip\a.bin"),
            record(2, r"C:\Users\Ada\keep.bin"),
            record(3, r"C:\Skip\deep\b.bin"),
            record(4, r"C:\Skipper\not-excluded.bin"),
        ];
        retain_not_excluded(&mut batch, &ex);
        let kept: Vec<u64> = batch.iter().map(|r| r.candidate_id.0).collect();
        assert_eq!(kept, vec![2, 4]);
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
                ..DetectorSettings::default()
            },
        );
        assert_eq!(runtime.apply_settings(&settings).unwrap(), 1);
        let records = runtime.index.get_all();
        assert_eq!(records[0].category, CategoryId::LargeFiles);
    }

    /// T-152 DoD: вимкнення детектора прибирає його категорію з індексу і
    /// з live totals (цифра «можна звільнити» та ряд Sidebar) без рескану.
    #[test]
    fn disabling_detector_clears_category_and_live_totals() {
        let runtime = ScanRuntime::new();
        runtime
            .index
            .insert_batch(vec![FileRecord {
                candidate_id: CandidateId(1),
                path: "C:\\huge.bin".into(),
                size: ByteSize(500 * 1024 * 1024),
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

        // Дефолтні settings: large_files увімкнено → файл категоризовано.
        runtime.apply_settings(&AppSettings::default()).unwrap();
        assert_eq!(runtime.index.get_all()[0].category, CategoryId::LargeFiles);
        assert!(runtime.last_totals.lock().unwrap().summary().unique_bytes.0 > 0);

        // Вимкнення детектора → категорія знята, цифра нуль.
        let mut settings = AppSettings::default();
        settings.detectors.insert(
            "large_files".into(),
            DetectorSettings {
                enabled: false,
                ..DetectorSettings::default()
            },
        );
        runtime.apply_settings(&settings).unwrap();
        assert_eq!(
            runtime.index.get_all()[0].category,
            CategoryId::Uncategorized
        );
        let summary = runtime.last_totals.lock().unwrap().summary();
        assert_eq!(summary.unique_bytes.0, 0, "цифра «можна звільнити» = 0");
        let large_idx = CategoryId::LargeFiles.mvp_index().unwrap();
        assert_eq!(
            summary.by_category[large_idx].bytes, 0,
            "ряд категорії без внеску"
        );
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

    /// T-152 fix: індекс із primary `duplicates_cascade` → `apply_settings`
    /// (дефолт) не знімає категорію Duplicates; live totals лишають внесок.
    /// also_in «Дублікати» для multi-hit (primary farm + cascade reap)
    /// теж відновлюється з кешу груп.
    #[test]
    fn apply_settings_preserves_duplicates_cascade_primary_and_live_totals() {
        use trashradar_domain::duplicates::{
            ContentHash, DuplicateRole, MarkedDuplicateGroup, MarkedDuplicateMember,
        };

        let cascade_size = 200_000u64;
        let large_size = 200 * 1024 * 1024u64;
        let runtime = ScanRuntime::new();

        // id=1: cascade-only primary (малий Other — ферма не матчить).
        // id=2: keep-екземпляр каскаду (не reclaimable).
        // id=3: multi-hit reap — primary LargeFiles + also_in Duplicates.
        runtime
            .index
            .insert_batch(vec![
                FileRecord {
                    candidate_id: CandidateId(1),
                    path: r"C:\media\copy-a.bin".into(),
                    size: ByteSize(cascade_size),
                    created_at: None,
                    modified_at: None,
                    accessed_at: None,
                    kind: FileKind::Other,
                    unit: CandidateUnit::File,
                    category: CategoryId::Duplicates,
                    safety: SafetyLevel::SafeToBulk,
                    decision: Decision::Undecided,
                    detector_id: DUPLICATES_CASCADE_DETECTOR_ID.into(),
                    explanation: "дублікат · 2 копії по 0.0 ГБ".into(),
                    attributes: FileAttributes::default(),
                },
                FileRecord {
                    candidate_id: CandidateId(2),
                    path: r"C:\media\huge-keep.bin".into(),
                    size: ByteSize(large_size),
                    created_at: None,
                    modified_at: None,
                    accessed_at: None,
                    kind: FileKind::Other,
                    unit: CandidateUnit::File,
                    category: CategoryId::LargeFiles,
                    safety: SafetyLevel::ReviewRecommended,
                    decision: Decision::Undecided,
                    detector_id: "large_files".into(),
                    explanation: "розмір 0.2 ГБ".into(),
                    attributes: FileAttributes::default(),
                },
                FileRecord {
                    candidate_id: CandidateId(3),
                    path: r"C:\media\huge-reap.bin".into(),
                    size: ByteSize(large_size),
                    created_at: None,
                    modified_at: None,
                    accessed_at: None,
                    kind: FileKind::Other,
                    unit: CandidateUnit::File,
                    category: CategoryId::LargeFiles,
                    safety: SafetyLevel::ReviewRecommended,
                    decision: Decision::Undecided,
                    detector_id: "large_files".into(),
                    explanation: "розмір 0.2 ГБ".into(),
                    attributes: FileAttributes::default(),
                },
            ])
            .unwrap();

        runtime.duplicates.lock().unwrap().groups = vec![MarkedDuplicateGroup {
            size: ByteSize(large_size),
            content_hash: ContentHash::ZERO,
            members: vec![
                MarkedDuplicateMember {
                    candidate_id: CandidateId(1),
                    path: r"C:\media\copy-a.bin".into(),
                    role: DuplicateRole::Reap,
                    keep: false,
                },
                MarkedDuplicateMember {
                    candidate_id: CandidateId(2),
                    path: r"C:\media\huge-keep.bin".into(),
                    role: DuplicateRole::Keep,
                    keep: true,
                },
                MarkedDuplicateMember {
                    candidate_id: CandidateId(3),
                    path: r"C:\media\huge-reap.bin".into(),
                    role: DuplicateRole::Reap,
                    keep: false,
                },
            ],
            keep_id: CandidateId(2),
            policy: KeepPolicy::default(),
            potential_reclaim_bytes: cascade_size + large_size,
        }];

        {
            let mut live = runtime.last_totals.lock().unwrap();
            live.clear();
            live.ingest_primary(&runtime.index.get_all());
        }
        let dup_idx = CategoryId::Duplicates.mvp_index().unwrap();
        assert_eq!(
            runtime.last_totals.lock().unwrap().summary().by_category[dup_idx].bytes,
            cascade_size
        );

        // Будь-яка зміна налаштувань (тут — дефолт) раніше знімала Duplicates.
        runtime.apply_settings(&AppSettings::default()).unwrap();

        let records = runtime.index.get_all();
        let primary = records
            .iter()
            .find(|r| r.candidate_id == CandidateId(1))
            .unwrap();
        assert_eq!(
            primary.category,
            CategoryId::Duplicates,
            "primary Duplicates збережено (CompactFileRecord не тримає detector_id)"
        );
        // detector_id/explanation у InMemoryIndex завжди "" після pack (T-015) —
        // не assert-имо їх; UI «Дублікати» бере групи з duplicates.groups, не
        // explanation з FileRecord.

        let large = records
            .iter()
            .find(|r| r.candidate_id == CandidateId(3))
            .unwrap();
        assert_eq!(large.category, CategoryId::LargeFiles);
        let also = runtime.also_in_categories(CandidateId(3), CategoryId::LargeFiles);
        assert!(
            also.contains(&CategoryId::Duplicates),
            "маркер «також у: Дублікати» відновлено з кешу груп, got {also:?}"
        );

        let after = runtime.last_totals.lock().unwrap().summary();
        assert_eq!(
            after.by_category[dup_idx].bytes, cascade_size,
            "live totals: внесок Duplicates збережено"
        );
        assert!(
            after.unique_bytes.0 >= cascade_size,
            "цифра «можна звільнити» містить cascade primary"
        );
    }

    /// Регресія: CompactFileRecord губить detector_id — захист clear має
    /// працювати за category=Duplicates, інакше apply_settings стирає primary
    /// навіть із заповненим detector_id на insert.
    #[test]
    fn apply_settings_preserves_duplicates_even_when_detector_id_stripped() {
        let size = 50_000u64;
        let runtime = ScanRuntime::new();
        runtime
            .index
            .insert_batch(vec![FileRecord {
                candidate_id: CandidateId(1),
                path: r"C:\dup.bin".into(),
                size: ByteSize(size),
                created_at: None,
                modified_at: None,
                accessed_at: None,
                kind: FileKind::Other,
                unit: CandidateUnit::File,
                category: CategoryId::Duplicates,
                safety: SafetyLevel::SafeToBulk,
                decision: Decision::Undecided,
                detector_id: DUPLICATES_CASCADE_DETECTOR_ID.into(),
                explanation: "дублікат".into(),
                attributes: FileAttributes::default(),
            }])
            .unwrap();

        // Після pack/unpack detector_id уже порожній — як у проді.
        let stripped = &runtime.index.get_all()[0];
        assert_eq!(stripped.category, CategoryId::Duplicates);
        assert!(
            stripped.detector_id.is_empty(),
            "передумова: hot index не персистить detector_id"
        );
        assert!(stripped.explanation.is_empty());

        // Без кешу груп: лише category-based skip у recalculate.
        assert!(runtime.duplicates.lock().unwrap().groups.is_empty());
        runtime.apply_settings(&AppSettings::default()).unwrap();

        let after = runtime.index.get_all();
        assert_eq!(after[0].category, CategoryId::Duplicates);
        let summary = runtime.last_totals.lock().unwrap().summary();
        let dup_idx = CategoryId::Duplicates.mvp_index().unwrap();
        assert_eq!(summary.by_category[dup_idx].bytes, size);
        assert_eq!(summary.unique_bytes.0, size);
    }
}
