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
  | "dev_artifacts";

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
  | "cancelled";

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
  | "app.ping"
  | "app.test_stream"
  | "app.request_elevation"
  | "app.decline_elevation"
  | "scan.start"
  | "scan.stop"
  | "category.window"
  | "category.set_threshold"
  | "candidate.mark"
  | "candidate.keep"
  | "reap.execute"
  | "reap.undo_batch"
  | "quarantine.restore_batch"
  | "quarantine.purge"
  | "settings.get"
  | "settings.set"
  | "search.candidates";

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
  | "quarantine.changed"
  | "quarantine.entry_expired"
  | "settings.changed";

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

/** Подія scan.progress (T-033). */
export interface ScanProgressEvent {
  volume: string;
  strategy: string;
  phase: string;
  filesIndexed: number;
  volumeIndex: number;
  volumeCount: number;
  done: boolean;
  cancelled: boolean;
}
