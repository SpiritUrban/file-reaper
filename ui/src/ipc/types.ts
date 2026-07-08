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

/** Відповідь команди app.health — єдиної команди каркаса. */
export interface HealthInfo {
  appVersion: string;
  coreStatus: string;
}

/** Імена команд (contracts/ipc-contract.json → commands). */
export type CommandName =
  | "app.health"
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
