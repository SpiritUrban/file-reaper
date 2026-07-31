//! Прев'ю-міст UI↔Core (T-120): перша IPC-поверхня над готовим Core-конвеєром
//! превью (T-067…T-076 — `PreviewScheduler`, ланцюжок `ThumbnailSource`,
//! `PngThumbnailEncoder`, ffmpeg-скраб). До цієї задачі жодна `preview.*`
//! команда не існувала — плитки показували лише гліфи-заглушки.
//!
//! Чотири команди (+ metrics у відповіді prefetch):
//! - `preview.thumbnail` — статична мініатюра плитки (T-100/T-111): кеш
//!   (T-068) синхронно, інакше генерація P1-задачею + подія `preview.ready`;
//! - `preview.scrub_strip` — скраб-смуга кадрів відео (T-072) на вимогу
//!   (перший hover плитки, T-120); не кешується на диску в цій версії —
//!   формат raw BGRA у `PreviewCacheManager` (T-068) не самоописовий за
//!   розмірами кадру, а конвеєр читання його назад ще не спроєктовано;
//!   клієнтський кеш у `ui/src/store/preview.ts` покриває DoD «без
//!   декодування на льоту» повторних скрабів у сесії;
//! - `preview.large` — велике превью панелі деталей (T-073 → T-124):
//!   обгортка над уже готовим `TwoStagePreviewer` — Draft із дискового
//!   кешу синхронно (<100 мс, DoD T-124), Sharp генерується P0-задачею у
//!   фоні й доставляється тією самою подією `preview.ready`;
//! - `preview.prefetch` — T-146 / інтеграція T-075: P2-прогрів наступних
//!   2–3 плиток за знаком траєкторії курсора; Draft для Live Preview
//!   правої зони береться з цього кешу мініатюр (architecture.md §5.3).

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use base64::Engine;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Runtime};

use trashradar_app::ports::{
    PreviewCache, PreviewCacheEntry, PreviewKind, RawThumbnail, ThumbnailEncoder, ThumbnailSource,
    VideoFrameSource, DEFAULT_SCRUB_FRAME_COUNT,
};
use trashradar_app::preview::{
    encode_thumbnail, generate_from_chain, PreviewCacheManager, PreviewDelivery, PreviewDeliveryFn,
    PreviewJobKind, PreviewPrefetchCandidate, PreviewPrefetcher, PreviewPriority, PreviewQuality,
    PreviewScheduler, TwoStageOutcome, TwoStagePreviewer,
};
use trashradar_domain::candidate::{ByteSize, CandidateId, CandidateUnit, FsTimestamp};
use trashradar_domain::error::CoreError;
use trashradar_preview::{FfmpegVideoFrameSource, ImageThumbnailSource, PngThumbnailEncoder};

use crate::events;
use crate::ipc::record_command;

/// Кеш превью-заглушка: профіль недоступний (тести / LOCALAPPDATA відсутня)
/// → повна деградація без кешу, а не крах (той самий принцип, що й
/// `read_profile_quarantine_badge` у `ipc.rs`).
struct NullPreviewCache;

impl PreviewCache for NullPreviewCache {
    fn get_preview_entry(
        &self,
        _source_path: &str,
        _kind: PreviewKind,
    ) -> Result<Option<PreviewCacheEntry>, CoreError> {
        Ok(None)
    }

    fn put_preview_entry(&self, _entry: &PreviewCacheEntry) -> Result<(), CoreError> {
        Ok(())
    }

    fn delete_preview_entry(
        &self,
        _source_path: &str,
        _kind: PreviewKind,
    ) -> Result<(), CoreError> {
        Ok(())
    }

    fn delete_all_previews_for_path(&self, _source_path: &str) -> Result<(), CoreError> {
        Ok(())
    }
}

/// Керований Tauri-стан прев'ю-конвеєра (кеш + черга + ланцюжок джерел).
pub struct PreviewRuntime {
    cache: Arc<PreviewCacheManager>,
    scheduler: Arc<PreviewScheduler>,
    chain: Vec<Arc<dyn ThumbnailSource>>,
    encoder: Arc<dyn ThumbnailEncoder>,
    video: Arc<FfmpegVideoFrameSource>,
    /// T-075/T-146: P2-прогрів 2–3 сусідів за траєкторією (спільний кеш/черга).
    prefetcher: Arc<PreviewPrefetcher>,
}

impl PreviewRuntime {
    pub fn new(profile: Option<PathBuf>) -> Self {
        let cache_dir = profile
            .as_ref()
            .map(|path| path.join("cache").join("previews"))
            .unwrap_or_else(|| std::env::temp_dir().join("trashradar-previews"));
        let db: Arc<dyn PreviewCache> = match &profile {
            Some(path) => match trashradar_index_sqlite::ThreadSafePreviewCache::open_profile(path)
            {
                Ok(db) => Arc::new(db),
                Err(error) => {
                    tracing::warn!(%error, "Preview cache DB недоступна — деградація без кешу");
                    Arc::new(NullPreviewCache)
                }
            },
            None => Arc::new(NullPreviewCache),
        };
        // Правило 29: багатозоновий пошук — завантажене 1-кліком у профілі,
        // ресурси бандла (розкладка своя на кожній платформі), поруч з exe,
        // далі PATH. Без цього укомплектований бінарник лежав би в бандлі
        // невидимим.
        let exe_dir = std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(PathBuf::from));
        let search_dirs =
            crate::ffmpeg_setup::ffmpeg_search_dirs(profile.as_deref(), exe_dir.as_deref());
        let video = Arc::new(FfmpegVideoFrameSource::with_search_dirs(&search_dirs));
        // Стартова діагностика: одразу видно у логах, чи бекенд знайшов ffmpeg
        // (без нього постер-кадр і скраб-смуга відео недоступні — T-071).
        match video.binary_path() {
            Some(path) => {
                tracing::info!(target: "preview", path = %path.display(), "ffmpeg знайдено — відео-превʼю (постер + скраб) активні");
            }
            None => {
                tracing::warn!(
                    target: "preview",
                    env_var = trashradar_preview::FFMPEG_ENV_VAR,
                    searched = search_dirs.len(),
                    "ffmpeg НЕ знайдено — відео показуватимуть лише іконку. У Налаштуваннях є кнопка завантаження в 1 клік"
                );
            }
        }
        // Ланцюжок за спаданням швидкості (architecture.md §5.2): системний
        // кеш мініатюр Windows → власне декодування зображень → ключовий
        // кадр відео (VideoKeyFrameSource — той самий міст, що й T-073).
        let chain: Vec<Arc<dyn ThumbnailSource>> = vec![
            Arc::new(trashradar_preview::WindowsShellThumbnailSource::new()),
            Arc::new(ImageThumbnailSource::new()),
            Arc::new(trashradar_app::preview::VideoKeyFrameSource(
                Arc::clone(&video) as Arc<dyn VideoFrameSource>,
            )),
            // Остання ланка: асоційована іконка (значок .exe / типу файла) —
            // для коду/інсталяторів/документів без справжньої мініатюри плитка
            // показує фірмову іконку замість generic-гліфа.
            Arc::new(trashradar_preview::WindowsShellIconSource::new()),
        ];
        let cache = Arc::new(PreviewCacheManager::new(cache_dir, db));
        // 2 воркери: превью — фонове навантаження, не має конкурувати
        // з IPC-потоком чи скан-воркерами (architecture.md §13).
        let scheduler = Arc::new(PreviewScheduler::new(2, 0.05));
        let encoder: Arc<dyn ThumbnailEncoder> = Arc::new(PngThumbnailEncoder::new());
        // look_ahead=3, max_edge=160 — дефолти плитки (T-075 / T-120).
        let prefetcher = Arc::new(PreviewPrefetcher::new(
            Arc::clone(&cache),
            Arc::clone(&scheduler),
            chain.clone(),
            Arc::clone(&encoder),
            160,
            3,
        ));
        Self {
            cache,
            scheduler,
            chain,
            encoder,
            video,
            prefetcher,
        }
    }

    /// Джерело кадрів відео: спільний шлях до ffmpeg, який 1-клік
    /// завантаження підміняє на місці, без перезапуску (правило 29).
    pub fn video(&self) -> FfmpegVideoFrameSource {
        (*self.video).clone()
    }

    /// Клон дескриптора кешу для перенесення у `spawn_blocking` (architecture.md §14:
    /// будь-який дисковий I/O — асинхронний навіть для маленького файла кешу).
    fn cache_handle(&self) -> Arc<PreviewCacheManager> {
        Arc::clone(&self.cache)
    }

    /// Свіжий `TwoStagePreviewer` (T-073) над спільними кешем/чергою/ланцюжком
    /// (T-124) — сам він не тримає стану, лише переносить наявні `Arc`.
    fn two_stage_previewer(&self) -> TwoStagePreviewer {
        TwoStagePreviewer::new(
            Arc::clone(&self.cache),
            Arc::clone(&self.scheduler),
            self.chain.clone(),
            Arc::clone(&self.encoder),
        )
    }

    /// Запланувати генерацію мініатюри P1-задачею; доставка — подія
    /// `preview.ready` (T-067 черга, T-074 панік-бар'єр уже в
    /// `generate_from_chain`/`encode_thumbnail`).
    fn schedule_thumbnail<R: Runtime>(
        &self,
        app: AppHandle<R>,
        path: String,
        size: ByteSize,
        modified_at: Option<FsTimestamp>,
        max_edge: u32,
    ) {
        let cache = Arc::clone(&self.cache);
        let chain = self.chain.clone();
        let encoder = Arc::clone(&self.encoder);
        // PreviewJobKind::Thumbnail — окремий слот від Large (P0): інакше
        // hover одночасно з preview.large витісняв би мініатюру (або навпаки).
        self.scheduler.submit_job(
            path.clone(),
            PreviewJobKind::Thumbnail,
            PreviewPriority::P1,
            move |token| {
                if token.is_cancelled() {
                    return;
                }
                // T-120 діагностика: ланцюжок джерел мовчки повертав None при
                // будь-якому збої (не-зображення, битий файл, недоступний шлях),
                // і плитка залишалась порожньою без жодного сліду. Логуємо
                // конкретну причину, щоб «пусті площини» стали діагностованими.
                let Some(raw) = generate_from_chain(&chain, &path, max_edge) else {
                    tracing::warn!(
                        target: "preview",
                        %path,
                        "мініатюру не згенеровано: жодне джерело ланцюжка не покрило файл (не-зображення/битий/недоступний)"
                    );
                    return;
                };
                if token.is_cancelled() {
                    return;
                }
                let Some(bytes) = encode_thumbnail(encoder.as_ref(), &raw) else {
                    tracing::warn!(target: "preview", %path, "кодування PNG-мініатюри провалилось");
                    return;
                };
                // Доставка НЕ залежить від успіху кешування: раніше подію
                // емітили лише при `save_preview().is_ok()`, тож недоступний
                // профіль/диск лишав плитку порожньою попри вдалу генерацію.
                // Кеш — оптимізація наступного запиту, не умова показу.
                if let Err(error) =
                    cache.save_preview(&path, PreviewKind::Thumbnail, size, modified_at, &bytes)
                {
                    tracing::debug!(
                        target: "preview",
                        %path,
                        %error,
                        "запис мініатюри в кеш провалився (best-effort; показ не блокується)"
                    );
                }
                events::emit(
                    &app,
                    events::topic::PREVIEW_READY,
                    &events::PreviewReadyEvent {
                        path: path.clone(),
                        kind: "thumbnail".into(),
                        data_url: to_data_url(&bytes),
                    },
                );
            },
        );
    }
}

fn to_data_url(png_bytes: &[u8]) -> String {
    format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(png_bytes)
    )
}

/// `pub(crate)` — перевикористовується `candidate.reveal_in_explorer` (T-125,
/// `scan_runtime.rs`): той самий лукап за candidate_id, без дублювання.
pub(crate) fn find_record(
    scan: &crate::scan_runtime::ScanRuntime,
    candidate_id: u64,
) -> Result<trashradar_domain::candidate::FileRecord, CoreError> {
    scan.index
        .get_all()
        .into_iter()
        .find(|r| r.candidate_id == CandidateId(candidate_id))
        .ok_or_else(|| CoreError::invalid_argument("Кандидата не знайдено в індексі."))
}

/// Параметри `preview.thumbnail`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewThumbnailPayload {
    pub candidate_id: u64,
    #[serde(default)]
    pub max_edge: Option<u32>,
}

/// Відповідь `preview.thumbnail`: або готовий `dataUrl` з кешу, або
/// підтвердження, що генерація заплановано — фінал прийде подією
/// `preview.ready` (architecture.md §1.2, команда неблокуюча).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewThumbnailAck {
    /// `"cached"` — `dataUrl` заповнено; `"scheduled"` — чекати `preview.ready`.
    pub status: String,
    pub data_url: Option<String>,
}

/// Кеш синхронно, інакше P1-генерація в фоні через ланцюжок джерел
/// (architecture.md §5.2) + доставка подією `preview.ready`. Спільне тіло
/// `preview.thumbnail` (T-120, за candidate_id/HotIndex) і `quarantine.thumbnail`
/// (T-130, за сурогатним шляхом карантину) — обидва мають один і той самий
/// «кеш або заплануй» конвеєр, різниться лише звідки взявся `path`.
async fn thumbnail_for_path<R: Runtime>(
    app: AppHandle<R>,
    preview: &PreviewRuntime,
    path: String,
    size: ByteSize,
    modified_at: Option<FsTimestamp>,
    max_edge: u32,
) -> Result<PreviewThumbnailAck, CoreError> {
    let cache = preview.cache_handle();
    let lookup_path = path.clone();
    let cached = tauri::async_runtime::spawn_blocking(move || {
        let cached_path = cache.get_valid_preview_path(
            &lookup_path,
            PreviewKind::Thumbnail,
            size,
            modified_at,
        )?;
        std::fs::read(cached_path).ok()
    })
    .await
    .map_err(|error| CoreError::internal(format!("Preview cache lookup task failed: {error}")))?;

    if let Some(bytes) = cached {
        return Ok(PreviewThumbnailAck {
            status: "cached".into(),
            data_url: Some(to_data_url(&bytes)),
        });
    }

    preview.schedule_thumbnail(app, path, size, modified_at, max_edge);
    Ok(PreviewThumbnailAck {
        status: "scheduled".into(),
        data_url: None,
    })
}

/// Статична мініатюра плитки (T-100/T-111 → T-120): кеш синхронно,
/// інакше P1-генерація в фоні через ланцюжок джерел (architecture.md §5.2).
#[tauri::command]
pub async fn preview_thumbnail<R: Runtime>(
    app: AppHandle<R>,
    payload: PreviewThumbnailPayload,
    scan: tauri::State<'_, crate::scan_runtime::ScanRuntime>,
    preview: tauri::State<'_, PreviewRuntime>,
) -> Result<PreviewThumbnailAck, CoreError> {
    record_command();
    let record = find_record(&scan, payload.candidate_id)?;
    if record.unit != trashradar_domain::candidate::CandidateUnit::File {
        // Папка-одиниця (T-053) — немає єдиного файла для мініатюри.
        return Ok(PreviewThumbnailAck {
            status: "unavailable".into(),
            data_url: None,
        });
    }
    let max_edge = payload.max_edge.unwrap_or(160).clamp(16, 1024);
    thumbnail_for_path(
        app,
        &preview,
        record.path.clone(),
        record.size,
        record.modified_at,
        max_edge,
    )
    .await
}

/// Параметри `quarantine.thumbnail`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuarantineThumbnailPayload {
    pub entry_id: u64,
    #[serde(default)]
    pub max_edge: Option<u32>,
}

/// Мініатюра плитки Quarantine (T-130): той самий кеш+ланцюжок джерел, що й
/// `preview.thumbnail`, але джерело — сурогатний шлях карантину
/// (`quarantine-fs::surrogate_path`), не HotIndex: файл уже переміщений з
/// оригінального шляху (T-088 прибирає його з індексу під час reap-rename),
/// тому лукап іде через manifest (`ipc::find_quarantine_entry`), не `find_record`.
/// `modified_at: None` — карантинні файли не змінюються на місці; валідність
/// кешу за розміром (унікальний сурогатний шлях і так не колізує).
#[tauri::command]
pub async fn quarantine_thumbnail<R: Runtime>(
    app: AppHandle<R>,
    payload: QuarantineThumbnailPayload,
    profile: tauri::State<'_, crate::ipc::ProfileRuntime>,
    preview: tauri::State<'_, PreviewRuntime>,
) -> Result<PreviewThumbnailAck, CoreError> {
    record_command();
    let profile_dir = profile.profile_dir();
    let entry_id = payload.entry_id;
    let entry = tauri::async_runtime::spawn_blocking(move || {
        crate::ipc::find_quarantine_entry(profile_dir, entry_id)
    })
    .await
    .map_err(|error| {
        CoreError::internal(format!("Quarantine entry lookup task failed: {error}"))
    })??;

    let surrogate = trashradar_quarantine_fs::NativeQuarantineFs
        .surrogate_path(&entry.original_path, &entry.surrogate_name)?;
    let max_edge = payload.max_edge.unwrap_or(160).clamp(16, 1024);
    thumbnail_for_path(
        app,
        &preview,
        surrogate.to_string_lossy().into_owned(),
        entry.size,
        None,
        max_edge,
    )
    .await
}

/// Параметри `preview.scrub_strip`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewScrubStripPayload {
    pub candidate_id: u64,
    #[serde(default)]
    pub max_edge: Option<u32>,
    #[serde(default)]
    pub frames: Option<u32>,
}

/// Відповідь `preview.scrub_strip`: кадри в порядку таймлайну; порожній
/// масив — не відео/декодер недоступний (UI лишається на статичній мініатюрі).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewScrubStripAck {
    pub frame_count: u32,
    pub frames: Vec<String>,
}

/// Скраб-смуга кадрів відео на вимогу (T-072 → T-120: рух курсора = кадри).
/// Один прохід ffmpeg (T-072 DoD); не в P0/P1-бюджеті §15 — запитується
/// лише при першому наведенні на відео-плитку, клієнтський кеш (T-120 UI)
/// покриває повторні наведення без повторного виклику.
#[tauri::command]
pub async fn preview_scrub_strip(
    payload: PreviewScrubStripPayload,
    scan: tauri::State<'_, crate::scan_runtime::ScanRuntime>,
    preview: tauri::State<'_, PreviewRuntime>,
) -> Result<PreviewScrubStripAck, CoreError> {
    record_command();
    let record = find_record(&scan, payload.candidate_id)?;
    let max_edge = payload.max_edge.unwrap_or(480).clamp(64, 1920);
    let frame_count = payload
        .frames
        .unwrap_or(DEFAULT_SCRUB_FRAME_COUNT)
        .clamp(2, 20);

    let video = Arc::clone(&preview.video);
    let encoder = Arc::clone(&preview.encoder);
    let path = record.path;

    tauri::async_runtime::spawn_blocking(move || {
        let strip = video
            .scrub_strip(&path, max_edge, frame_count)
            .ok()
            .flatten();
        match strip {
            None => PreviewScrubStripAck {
                frame_count: 0,
                frames: Vec::new(),
            },
            Some(strip) => {
                let mut frames = Vec::with_capacity(strip.frame_count as usize);
                for index in 0..strip.frame_count {
                    let Some(slice) = strip.frame_slice(index) else {
                        continue;
                    };
                    let raw = RawThumbnail {
                        width: strip.frame_width,
                        height: strip.frame_height,
                        bgra: slice.to_vec(),
                    };
                    if let Ok(bytes) = encoder.encode(&raw) {
                        frames.push(to_data_url(&bytes));
                    }
                }
                PreviewScrubStripAck {
                    frame_count: frames.len() as u32,
                    frames,
                }
            }
        }
    })
    .await
    .map_err(|error| CoreError::internal(format!("Scrub strip task failed: {error}")))
}

/// Параметри `preview.large`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewLargePayload {
    pub candidate_id: u64,
    #[serde(default)]
    pub max_edge: Option<u32>,
}

/// Відповідь `preview.large`: усе, що `TwoStagePreviewer` доставив
/// **синхронно** в межах цього виклику (T-073 ступінь 1 — Draft з кешу,
/// або вже готовий Sharp). Будь-яка пізніша доставка (P0-генерація Sharp
/// у фоні) — подія `preview.ready` з `kind: "large_sharp"`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewLargeAck {
    /// `sharp_from_cache` | `draft_then_sharp_scheduled` | `sharp_scheduled_only` | `unavailable`.
    pub status: String,
    /// `"draft"` | `"sharp"` — якість `dataUrl`, якщо він є.
    pub quality: Option<String>,
    pub data_url: Option<String>,
}

fn two_stage_outcome_wire(outcome: TwoStageOutcome) -> &'static str {
    match outcome {
        TwoStageOutcome::SharpFromCache => "sharp_from_cache",
        TwoStageOutcome::DraftThenSharpScheduled => "draft_then_sharp_scheduled",
        TwoStageOutcome::SharpScheduledOnly => "sharp_scheduled_only",
    }
}

/// Велике превью панелі деталей (T-124): Draft миттєво з кешу (<100 мс,
/// architecture.md §5.3), Sharp — P0-задача у фоні з доставкою-підміною.
#[tauri::command]
pub async fn preview_large<R: Runtime>(
    app: AppHandle<R>,
    payload: PreviewLargePayload,
    scan: tauri::State<'_, crate::scan_runtime::ScanRuntime>,
    preview: tauri::State<'_, PreviewRuntime>,
) -> Result<PreviewLargeAck, CoreError> {
    record_command();
    let record = find_record(&scan, payload.candidate_id)?;
    if record.unit != CandidateUnit::File {
        // Папка-одиниця (T-053) — немає єдиного файла для превью.
        return Ok(PreviewLargeAck {
            status: "unavailable".into(),
            quality: None,
            data_url: None,
        });
    }
    // T-146: фіксуємо demand для hit rate префетчу T-075 (Draft = Thumbnail).
    let _prefetch_hit = preview
        .prefetcher
        .observe_demand(&PreviewPrefetchCandidate {
            source_path: record.path.clone(),
            size: record.size,
            modified_at: record.modified_at,
        });
    let max_edge = payload.max_edge.unwrap_or(1024).clamp(64, 4096);
    let path = record.path.clone();
    let size = record.size;
    let modified_at = record.modified_at;

    // Синхронна доставка (Draft і/або вже готовий Sharp з кешу) повертається
    // прямо у відповідь команди — жодного зайвого проходу через подію для
    // того, що вже доступно зараз (DoD «<100 мс»).
    //
    // Важливо: після того, як команда «запечатує» слот і забирає first,
    // **усі** подальші доставки (P0-генерація Sharp у фоні) мають іти
    // подією `preview.ready`. Раніше колбек дивився лише `first.is_none()`:
    // після `take()` слот знову був порожнім → Sharp записувався в
    // покинутий mutex замість події → права зона Live Preview / панель
    // деталей лишались порожніми назавжди (cold large ніколи не доходив).
    let slot: Arc<Mutex<LargeSyncSlot>> = Arc::new(Mutex::new(LargeSyncSlot {
        first: None,
        sealed: false,
    }));
    let slot_for_cb = Arc::clone(&slot);
    let app_for_cb = app.clone();
    let on_delivery: PreviewDeliveryFn = Arc::new(move |delivery: PreviewDelivery| {
        let quality = match delivery.quality {
            PreviewQuality::Draft => "draft",
            PreviewQuality::Sharp => "sharp",
        };
        let Ok(bytes) = std::fs::read(&delivery.preview_path) else {
            return;
        };
        let data_url = to_data_url(&bytes);
        {
            let mut guard = slot_for_cb.lock().expect("large sync slot lock");
            if !guard.sealed && guard.first.is_none() {
                guard.first = Some((quality, data_url));
                return;
            }
        }
        events::emit(
            &app_for_cb,
            events::topic::PREVIEW_READY,
            &events::PreviewReadyEvent {
                path: delivery.source_path.clone(),
                kind: format!("large_{quality}"),
                data_url,
            },
        );
    });

    let previewer = preview.two_stage_previewer();
    let path_for_blocking = path.clone();
    let outcome = tauri::async_runtime::spawn_blocking(move || {
        previewer.request_large_preview(
            &path_for_blocking,
            size,
            modified_at,
            max_edge,
            on_delivery,
        )
    })
    .await
    .map_err(|error| CoreError::internal(format!("Large preview task failed: {error}")))??;

    // Запечатуємо під тим самим локом, що й take: будь-яка concurrent
    // доставка з воркера або чекає lock і бачить sealed=true → подія,
    // або вже емітнула, поки first був зайнятий.
    let sync = {
        let mut guard = slot.lock().expect("large sync slot lock");
        guard.sealed = true;
        guard.first.take()
    };
    Ok(PreviewLargeAck {
        status: two_stage_outcome_wire(outcome).into(),
        quality: sync.as_ref().map(|(q, _)| (*q).to_string()),
        data_url: sync.map(|(_, d)| d),
    })
}

/// Слот синхронної доставки `preview.large` (див. `preview_large`).
struct LargeSyncSlot {
    /// Перша доставка в межах виклику (draft / sharp-from-cache).
    first: Option<(&'static str, String)>,
    /// `true` після того, як команда забрала `first` у відповідь — далі лише події.
    sealed: bool,
}

/// Параметри `preview.prefetch` (T-146 / T-075).
///
/// `candidate_ids` — упорядкований список плиток (видиме вікно сітки);
/// `anchor_index` — індекс плитки під курсором; `motion` — знак траєкторії
/// (+ уперед, − назад). Prefetcher (T-075) ставить P2 на наступні 2–3.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewPrefetchPayload {
    pub candidate_ids: Vec<u64>,
    pub anchor_index: usize,
    /// Знак руху: >0 уперед по списку, <0 назад. `0` = no-op.
    pub motion: i32,
}

/// Відповідь `preview.prefetch`: щойно заплановані шляхи + кумулятивні
/// метрики хітрейту (DoD T-146 «замір хітрейту префетчу»).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewPrefetchAck {
    pub scheduled_count: u32,
    pub scheduled_paths: Vec<String>,
    pub metrics: PreviewPrefetchMetricsDto,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewPrefetchMetricsDto {
    pub demands: u64,
    pub cache_hits: u64,
    pub scheduled: u64,
    /// 0.0…1.0; 0 якщо demands == 0.
    pub hit_rate: f64,
}

/// T-146: префетч за траєкторією — тонкий IPC над `PreviewPrefetcher` (T-075).
/// UI передає порядок плиток і напрям; Core резолвить шляхи з HotIndex і
/// ставить P2-задачі (не конкурують з P0 Live Preview / P1 видимих плиток).
#[tauri::command]
pub async fn preview_prefetch(
    payload: PreviewPrefetchPayload,
    scan: tauri::State<'_, crate::scan_runtime::ScanRuntime>,
    preview: tauri::State<'_, PreviewRuntime>,
) -> Result<PreviewPrefetchAck, CoreError> {
    record_command();
    if payload.candidate_ids.is_empty() {
        let m = preview.prefetcher.metrics();
        return Ok(PreviewPrefetchAck {
            scheduled_count: 0,
            scheduled_paths: Vec::new(),
            metrics: metrics_dto(m),
        });
    }
    if payload.anchor_index >= payload.candidate_ids.len() {
        return Err(CoreError::invalid_argument(
            "anchorIndex поза межами candidateIds.",
        ));
    }
    if payload.motion == 0 {
        let m = preview.prefetcher.metrics();
        return Ok(PreviewPrefetchAck {
            scheduled_count: 0,
            scheduled_paths: Vec::new(),
            metrics: metrics_dto(m),
        });
    }
    // Жорстка стеля: UI не повинен гнати всю категорію (T-014 вікна);
    // 256 — запас на віртуальну сітку + overscan, не 100k.
    if payload.candidate_ids.len() > 256 {
        return Err(CoreError::invalid_argument(
            "candidateIds занадто довгий для prefetch (макс. 256).",
        ));
    }

    let index = scan.index.get_all();
    let by_id: std::collections::HashMap<u64, _> =
        index.into_iter().map(|r| (r.candidate_id.0, r)).collect();

    // Лише file-одиниці (папки T-053 без єдиного превью); зберігаємо
    // оригінальні індекси UI-списку, щоб коректно змапити anchor.
    let mut candidates: Vec<PreviewPrefetchCandidate> =
        Vec::with_capacity(payload.candidate_ids.len());
    let mut original_indices: Vec<usize> = Vec::with_capacity(payload.candidate_ids.len());
    for (list_i, id) in payload.candidate_ids.iter().enumerate() {
        let Some(record) = by_id.get(id) else {
            continue; // сітка може випередити індекс
        };
        if record.unit != CandidateUnit::File {
            continue;
        }
        candidates.push(PreviewPrefetchCandidate {
            source_path: record.path.clone(),
            size: record.size,
            modified_at: record.modified_at,
        });
        original_indices.push(list_i);
    }

    if candidates.is_empty() {
        let m = preview.prefetcher.metrics();
        return Ok(PreviewPrefetchAck {
            scheduled_count: 0,
            scheduled_paths: Vec::new(),
            metrics: metrics_dto(m),
        });
    }

    // Точний збіг anchor → file-індекс; інакше найближчий файл ліворуч
    // (або перший праворуч), щоб motion все одно прогрів сусідів.
    let anchor_mapped = original_indices
        .iter()
        .position(|&i| i == payload.anchor_index)
        .or_else(|| {
            original_indices
                .iter()
                .rposition(|&i| i < payload.anchor_index)
        })
        .or_else(|| {
            original_indices
                .iter()
                .position(|&i| i > payload.anchor_index)
        })
        .unwrap_or(0);

    let prefetcher = Arc::clone(&preview.prefetcher);
    let motion = payload.motion;
    let scheduled = tauri::async_runtime::spawn_blocking(move || {
        prefetcher.prefetch_from_motion(&candidates, anchor_mapped, motion)
    })
    .await
    .map_err(|error| CoreError::internal(format!("Prefetch task failed: {error}")))?;

    let m = preview.prefetcher.metrics();
    Ok(PreviewPrefetchAck {
        scheduled_count: scheduled.len() as u32,
        scheduled_paths: scheduled,
        metrics: metrics_dto(m),
    })
}

fn metrics_dto(m: trashradar_app::preview::PreviewPrefetchMetrics) -> PreviewPrefetchMetricsDto {
    PreviewPrefetchMetricsDto {
        demands: m.demands,
        cache_hits: m.cache_hits,
        scheduled: m.scheduled,
        hit_rate: m.hit_rate(),
    }
}

/// Стан ffmpeg для UI: чи працюють відео-превʼю і чи є що качати
/// (правило 29 брифу Стадії 2).
#[tauri::command]
pub async fn ffmpeg_status(
    preview: tauri::State<'_, PreviewRuntime>,
) -> Result<crate::ffmpeg_setup::FfmpegStatus, CoreError> {
    record_command();
    Ok(crate::ffmpeg_setup::status_for(&preview.video()))
}

/// 1-клік завантаження ffmpeg у профіль користувача.
///
/// Довга операція, але з очевидним прогресом для людини (вона щойно
/// натиснула кнопку і чекає), тож окремої події не заводимо — команда
/// повертає результат, коли бінарник уже перевірений запуском.
#[tauri::command]
pub async fn ffmpeg_download(
    preview: tauri::State<'_, PreviewRuntime>,
) -> Result<crate::ffmpeg_setup::FfmpegStatus, CoreError> {
    record_command();
    let video = preview.video();
    let profile = crate::logging::data_profile_dir();
    crate::ffmpeg_setup::download(profile, video)
        .await
        .map_err(CoreError::internal)
}
