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

/** Головна цифра продукту + розбивка (подія cleanup.total_updated). */
export interface CleanupTotal {
  /** Сума УНІКАЛЬНИХ кандидатів — чесна цифра (T-054). */
  reclaimableBytes: number;
  categories: CategorySummary[];
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

/** Імена команд (contracts/ipc-contract.json → commands). */
export type CommandName =
  | "app.health"
  | "app.ping"
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
  | "scan.progress"
  | "cleanup.total_updated"
  | "category.updated"
  | "preview.ready"
  | "quarantine.changed"
  | "quarantine.entry_expired"
  | "settings.changed";
