/**
 * Типи контракту IPC — дзеркало contracts/ipc-contract.json.
 *
 * Тимчасово ведуться вручну; кодогенерація з contracts/ — задача
 * T-004/T-005 (docs/tasks.md). Термінологія = docs/product.md:
 * candidate, category, detector, reap, keep, quarantine.
 */

/** Дев'ять категорій-детекторів MVP (дзеркало domain::CategoryId). */
export type CategoryId =
  | "large_files"
  | "old_files"
  | "forgotten_videos"
  | "duplicates"
  | "archives"
  | "installers"
  | "temp_files"
  | "app_caches"
  | "dev_artifacts"
  | "empty_folders"
  | "sparse_folders"
  | "deep_paths";

export type FileKind =
  | "video"
  | "image"
  | "audio"
  | "archive"
  | "installer"
  | "disk_image"
  | "document"
  | "other";

export type SafetyLevel = "safe_to_bulk" | "review_recommended";

export type Decision = "undecided" | "keep" | "marked";

/** Кандидат на видалення — одиниця сітки (файл або папка-одиниця). */
export interface Candidate {
  id: number;
  path: string;
  kind: FileKind;
  unit: "file" | "folder";
  sizeBytes: number;
  lastAccessAt: string; // ISO 8601
  /** `null` — Core не зміг прочитати дату створення (T-125). */
  createdAt: string | null; // ISO 8601 | null
  decision: Decision;
  /** Пояснення детектора: «відео 4.2 ГБ, останній доступ 8 міс тому». */
  explanation: string;
  /** Інші категорії, куди входить файл (маркер «також у: …», T-121). */
  alsoIn: CategoryId[];
}

/** Живий агрегат категорії для Sidebar і Cleanup Summary. */
export interface CategorySummary {
  id: CategoryId;
  totalBytes: number;
  itemCount: number;
  safety: SafetyLevel;
  /** Дублікати рахуються групами, решта — файлами/папками. */
  countUnit: "files" | "groups" | "folders";
}

/** Головна цифра продукту + розбивка (подія cleanup.total_updated, T-055). */
export interface CleanupTotal {
  /** Сума УНІКАЛЬНИХ кандидатів — чесна цифра (T-054). */
  reclaimableBytes: number;
  /** Кількість унікальних reclaimable-файлів. */
  uniqueFiles: number;
  categories: CategorySummary[];
}

/** Подія category.updated (T-055) — один рядок Sidebar. */
export type CategoryUpdatedEvent = CategorySummary;

/** Мініпревью для 4–6 найбільших кандидатів у ряду Cleanup Summary (T-111). */
export interface CandidatePreview {
  id: number;
  kind: FileKind;
  sizeBytes: number;
}

/** Вікно кандидатів категорії з топ-файлами за розміром (запит T-111). */
export interface CategoryWindow {
  categoryId: CategoryId;
  /** Топ-кандидати за спаданням розміру (4–6 найбільших). */
  topCandidates: CandidatePreview[];
}

/** Політика «який залишити» у групі дублікатів (T-065). */
export type KeepPolicy =
  | "prefer_oldest_modified"
  | "prefer_newest_modified"
  | "prefer_outside_downloads"
  | "prefer_shortest_path";

/** Роль екземпляра: ✓ keep / ╳ reap (T-065 / T-126). */
export type DuplicateRole = "keep" | "reap";

/** Член групи з готовою розміткою з Core. */
export interface MarkedDuplicateMember {
  candidateId: number;
  path: string;
  role: DuplicateRole;
  /** true = ✓, false = ╳ */
  keep: boolean;
}

/** Підтверджена група дублікатів з дефолтною розміткою (T-065). */
export interface MarkedDuplicateGroup {
  size: number;
  contentHash: string;
  members: MarkedDuplicateMember[];
  keepId: number;
  policy: KeepPolicy;
  potentialReclaimBytes: number;
}

/**
 * Подія duplicates.cascade_updated (T-061).
 * Після stage 2: confidence=preliminary, refining=true («уточнюється»).
 * Після stage 3: confidence=confirmed, refining=false.
 */
export interface DuplicatesCascadeEvent {
  phase:
    | "idle"
    | "size_grouping"
    | "partial_hashing"
    | "full_hashing"
    | "complete"
    | "cancelled";
  confidence: "preliminary" | "confirmed";
  /** Маркер «уточнюється» до завершення повного хешу. */
  refining: boolean;
  reclaimableBytes: number;
  groupCount: number;
  filesInGroups: number;
  cancelled: boolean;
}

/** Відповідь `duplicates.groups` (T-126): останній кешований результат каскаду. */
export interface DuplicatesGroupsAck {
  state: DuplicatesCascadeEvent;
  groups: MarkedDuplicateGroup[];
}

export interface ScanProgress {
  volume: string;
  strategy: "mft" | "directory_walk" | "usn_delta";
  phase: "idle" | "full" | "incremental" | "cancelled" | "completed";
  percent: number;
}

export interface QuarantineEntry {
  id: number;
  batchId: number;
  originalPath: string;
  sizeBytes: number;
  quarantinedAt: string; // ISO 8601
  expiresAt: string; // ISO 8601
  status: "in_flight" | "quarantined" | "restored" | "purged";
}

/**
 * Стабільні коди помилок Core (дзеркало domain::ErrorCode, T-007).
 * UI розгалужується ЛИШЕ за кодом — текст показується як є.
 * Коди додаються, але ніколи не змінюються і не видаляються.
 */
export type CoreErrorCode =
  | "internal"
  | "invalid_argument"
  | "not_implemented"
  | "io"
  | "cancelled"
  | "path_protected"
  | "file_changed"
  | "quarantine_unavailable";

/** Конверт помилки Core → UI: {"code", "message"}. */
export interface CoreErrorPayload {
  code: CoreErrorCode;
  /** Людський текст — повне речення, готове до показу користувачу. */
  message: string;
}

/** Відповідь команди app.health. */
export interface HealthInfo {
  appVersion: string;
  coreStatus: string;
  modules: ModuleHealth[];
  ipc: IpcMetrics;
  /** Процес з адмін-правами (T-028). */
  elevated: boolean;
  /** Автовибір MFT ↔ walk по томах (T-028). */
  scanPlans: VolumeScanPlan[];
  /** Пропозиція / відмова elevation (T-034). */
  elevation: ElevationInfo;
  /** Доступність карантину по томах (T-089): reap блокується з причиною. */
  quarantineVolumes: VolumeQuarantineInfo[];
}

/** Доступність карантину на томі (T-089). */
export interface VolumeQuarantineInfo {
  /** Літера тому з двокрапкою, напр. "C:". */
  volume: string;
  /** true → службовий каталог записуваний, reap дозволений. */
  available: boolean;
  /** Людське пояснення блокування reap (лише коли available=false). */
  reason?: string;
}

/** Сесійний стан elevation (T-034). */
export interface ElevationInfo {
  /** elevated | not_needed | offer | declined */
  status: "elevated" | "not_needed" | "offer" | "declined" | string;
  /** Чи UI має показувати банер з поясненням. */
  offerPending: boolean;
  declinedThisSession: boolean;
  /** Пояснення вигоди (українською). */
  message: string;
  summary: string;
  ntfsVolumeCount: number;
}

/** Відповідь app.request_elevation (T-034). */
export interface RequestElevationReply {
  status: "started" | "already_elevated" | string;
  willExit: boolean;
}

/** Відповідь app.decline_elevation (T-034). */
export interface DeclineElevationReply {
  declined: boolean;
  offerPending: boolean;
}

export interface ModuleHealth {
  name: string;
  status: "online" | "planned" | "degraded" | "offline";
}

export interface IpcMetrics {
  commandsReceived: number;
  commandErrors: number;
  eventsEmitted: number;
  eventErrors: number;
}

/** План стратегії скану тому (T-028). */
export interface VolumeScanPlan {
  volume: string;
  strategy: "mft" | "directory_walk" | "usn_delta" | string;
  reason: "ntfs_elevated" | "not_ntfs" | "not_elevated" | string;
  fileSystem: string;
  elevated: boolean;
}

/** Параметри app.ping (усе опційне: {} — миттєвий успіх). */
export interface PingPayload {
  /** Штучна затримка відповіді, мс — перевірка неблокування UI. */
  delayMs?: number;
  /** Запросити відмову — перевірка конверта помилок наскрізь. */
  fail?: boolean;
}

/** Відповідь app.ping. */
export interface PingReply {
  version: string;
  delayedMs: number;
}

/** Параметри app.test_stream (діагностика каналу подій). */
export interface TestStreamPayload {
  /** Скільки подій надіслати (1..=1000, дефолт 5). */
  count?: number;
  /** Пауза між подіями, мс (0..=1000, дефолт 50). */
  intervalMs?: number;
}

/** Неблокуюче підтвердження прийняття app.test_stream. */
export interface TestStreamAck {
  accepted: number;
}

/** Подія діагностичного потоку (топік app.test). */
export interface TestEvent {
  seq: number;
  of: number;
}

/** Aggregated diagnostic counter event for T-006 throttling checks. */
export interface CounterSnapshot {
  delta: number;
  total: number;
}

/** Імена команд (contracts/ipc-contract.json → commands). */
export type CommandName =
  | "app.health"
  | "app.state"
  | "app.ping"
  | "app.test_stream"
  | "app.request_elevation"
  | "app.decline_elevation"
  | "scan.start"
  | "scan.stop"
  | "category.window"
  | "category.set_threshold"
  | "category.top_candidates"
  | "category.all_candidates"
  | "candidate.mark"
  | "candidate.keep"
  | "candidate.reveal_in_explorer"
  | "candidate.batch"
  | "reap.execute"
  | "reap.undo_batch"
  | "quarantine.restore_batch"
  | "quarantine.reveal_path"
  | "quarantine.purge"
  | "quarantine.window"
  | "quarantine.thumbnail"
  | "settings.get"
  | "settings.set"
  | "cache.get_usage"
  | "cache.clear"
  | "preview.thumbnail"
  | "preview.scrub_strip"
  | "preview.large"
  | "preview.prefetch"
  | "search.candidates"
  | "duplicates.groups";

export interface CacheUsage {
  bytes: number;
  files: number;
}

/** Відповідь preview.thumbnail (T-120). */
export interface PreviewThumbnailAck {
  status: "cached" | "scheduled" | "unavailable";
  /** `data:image/png;base64,…`; заповнено лише коли status="cached". */
  dataUrl?: string | null;
}

/** Відповідь preview.scrub_strip (T-120): кадри в порядку таймлайну. */
export interface PreviewScrubStripAck {
  frameCount: number;
  frames: string[];
}

/** Відповідь preview.large (T-124): усе, що доставлено синхронно (draft/sharp з кешу). */
export interface PreviewLargeAck {
  status:
    | "sharp_from_cache"
    | "draft_then_sharp_scheduled"
    | "sharp_scheduled_only"
    | "unavailable";
  quality?: "draft" | "sharp" | null;
  dataUrl?: string | null;
}

/** Метрики префетчу T-075/T-146 (кумулятивні за сесію Core). */
export interface PreviewPrefetchMetrics {
  demands: number;
  cacheHits: number;
  scheduled: number;
  /** 0…1; 0 якщо demands == 0. */
  hitRate: number;
}

/** Відповідь preview.prefetch (T-146): P2-прогрів сусідів + хітрейт. */
export interface PreviewPrefetchAck {
  scheduledCount: number;
  scheduledPaths: string[];
  metrics: PreviewPrefetchMetrics;
}

/** Імена подій (contracts/ipc-contract.json → events). */
export type EventName =
  | "app.test"
  | "app.test_counter"
  | "scan.journal_stale"
  | "index.updated"
  | "scan.progress"
  | "cleanup.total_updated"
  | "category.updated"
  | "duplicates.cascade_updated"
  | "preview.ready"
  | "quarantine.restored"
  | "quarantine.changed"
  | "quarantine.entry_expired"
  | "settings.changed";

/**
 * Подія preview.ready: фонова генерація завершилась. `"thumbnail"` — T-120
 * (P1-задача мініатюри плитки); `"large_sharp"` — T-124 (P0-задача великого
 * превью панелі деталей, підміняє draft).
 */
export interface PreviewReadyEvent {
  path: string;
  kind: "thumbnail" | "large_sharp";
  dataUrl: string;
}

/** Подія scan.journal_stale (T-031): USN застарів → повний рескан. */
export interface JournalStaleEvent {
  volume: string;
  reason: string;
  message: string;
  fullRescan: boolean;
}

/** Подія index.updated (T-032): Change Monitor застосував USN-дельту. */
export interface IndexUpdatedEvent {
  volume: string;
  created: number;
  modified: number;
  deleted: number;
  renamed: number;
}

/** Подія quarantine.restored (T-080): файл повернуто з карантину. */
export interface QuarantineRestoredEvent {
  entryId: number;
  originalPath: string;
  /** Фактичний шлях після відновлення (може мати суфікс). */
  restoredPath: string;
  /** Оригінальний шлях був зайнятий → застосовано суфікс. */
  usedSuffix: boolean;
  /** Попередження для показу — лише коли usedSuffix=true. */
  message?: string;
}

/** Відповідь `quarantine.restore_batch` (T-132): один запис на кожен entryId. */
export interface QuarantineRestoreOutcome {
  entryId: number;
  originalPath: string;
  restoredPath: string;
  usedSuffix: boolean;
}

/** Відповідь `quarantine.purge` (T-133). */
export interface QuarantinePurgeAck {
  purgedCount: number;
  purgedBytes: number;
}

/** Відповідь `reap.execute` (T-138) — batchId живить `reap.undo_batch`. */
export interface ReapExecuteAck {
  batchId: number;
  reapedCount: number;
  reapedBytes: number;
}

export interface AppSettings {
  quarantine: {
    ttlDays: number;
    warningThresholdBytes: number;
  };
  scan: {
    excludedPaths: string[];
    minimumSizeBytes: number;
    /** Поріг розділу «Майже порожні»: рекурсивно 1..=N файлів (дефолт 3). */
    sparseMaxFiles: number;
    /** Поріг розділу «Глибокі шляхи»: глибина папки понад це значення (дефолт 10). */
    deepPathMaxDepth: number;
    /** Літери томів, виключених з аналізу (напр. ["D"]). Порожньо = усі. */
    excludedVolumes: string[];
  };
  detectors: Record<
    string,
    {
      thresholds: Record<string, number>;
      /** T-152: перемикач детектора; відсутнє поле = увімкнено. */
      enabled?: boolean;
    }
  >;
}


export interface SettingsChangedEvent {
  settings: AppSettings;
  detectorRecordsRecalculated: number;
  quarantineRescheduled: boolean;
  scheduleGeneration: number;
}

/** Живе заповнення тому для блоку дисків Sidebar (T-106). */
export interface VolumeUsageInfo {
  /** Літера тому з двокрапкою, напр. "C:". */
  volume: string;
  capacityBytes: number;
  freeBytes: number;
}

/** Бейдж Quarantine у Sidebar (T-106): скільки зараз тримається. */
export interface QuarantineBadge {
  heldCount: number;
  heldBytes: number;
  /** Дата наступного автоочищення у UNIX-секундах, або 0 якщо карантин порожний (T-113). */
  nextPurgeAtUnix: number;
}

export interface AppStateSnapshot {
  cleanup: CleanupTotal;
  scanRunning: boolean;
  settings: AppSettings;
  /** Живі томи для смужок дисків (T-106). */
  volumes: VolumeUsageInfo[];
  /** Поточний вміст карантину для бейджа (T-106). */
  quarantine: QuarantineBadge;
  /** Перший запуск: чистий профіль без попередніх сканів (T-114). */
  isFirstRun: boolean;
}
export interface QuarantineChangedEvent {
  purgedCount: number;
  purgedBytes: number;
  heldBytes: number;
  thresholdExceeded: boolean;
  message?: string;
}

export interface QuarantineEntryExpiredEvent {
  entryId: number;
  originalPath: string;
  sizeBytes: number;
}

/** Параметри scan.start (T-033). */
export interface ScanStartPayload {
  /** Літери томів; omit = усі доступні. */
  volumes?: string[];
}

/** Ack scan.start. */
export interface ScanStartAck {
  accepted: boolean;
  volumeCount: number;
}

/** Ack scan.stop. */
export interface ScanStopAck {
  stopping: boolean;
}

/** Подія scan.progress (T-033 / лічильники індексу). */
export interface ScanProgressEvent {
  volume: string;
  strategy: string;
  phase: string;
  /** Усього файлів у сесії (усі томи + поточний). */
  filesIndexed: number;
  /** Файлів на поточному томі. */
  volumeFilesIndexed: number;
  /** Відомий total на томі; відсутній для MFT-стріму. */
  volumeFilesTotal?: number | null;
  /** MFT-записи / walk-об'єкти (росте, коли filesIndexed ще 0). */
  stageUnitsDone?: number;
  stageUnitsTotal?: number | null;
  /** mft_dirs | mft_files | walking | indexing | cascade | "" */
  stage?: string;
  volumeIndex: number;
  volumeCount: number;
  done: boolean;
  cancelled: boolean;
}
