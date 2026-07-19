/**
 * Відомі шляхи, які зазвичай / критично не варто видаляти.
 * Матчинг case-insensitive, Windows-шляхи.
 *
 * severity:
 * - critical — системні; видалення ламає Windows (червона ⚠ на плитці)
 * - caution  — службові/моделі/інструменти; краще не чіпати (напівпрозорі)
 */

export type KeepHintSeverity = "caution" | "critical";

export interface DoNotDeleteHint {
  /** Стабільний id правила (data-атрибути / дебаг). */
  id: string;
  /** Короткий опис на плитці. */
  label: string;
  severity: KeepHintSeverity;
}

function normalizePath(path: string): string {
  return path.replace(/\//g, "\\").toLowerCase();
}

/**
 * Повертає підказку «не видаляти», якщо шлях відомий.
 * Інакше `null` — плитка як звичайно.
 */
export function doNotDeleteHint(path: string): DoNotDeleteHint | null {
  const n = normalizePath(path);

  // ── critical: системні файли Windows у корені тома ───────────────
  if (/^[a-z]:\\pagefile\.sys$/.test(n)) {
    return {
      id: "windows-pagefile",
      label: "Файл підкачки Windows",
      severity: "critical",
    };
  }
  if (/^[a-z]:\\hiberfil\.sys$/.test(n)) {
    return {
      id: "windows-hiberfil",
      label: "Файл гібернації Windows",
      severity: "critical",
    };
  }
  if (/^[a-z]:\\swapfile\.sys$/.test(n)) {
    return {
      id: "windows-swapfile",
      label: "Swap-файл Windows",
      severity: "critical",
    };
  }
  if (/^[a-z]:\\dumpstack\.log\.tmp$/.test(n)) {
    return {
      id: "windows-dumpstack",
      label: "Системний журнал Windows",
      severity: "critical",
    };
  }
  if (/^[a-z]:\\memory\.dmp$/.test(n)) {
    return {
      id: "windows-memory-dmp",
      label: "Дамп пам’яті Windows",
      severity: "critical",
    };
  }

  // ── caution: браузерні on-device моделі (перекачаються) ──────────
  // Chrome Optimization Guide
  if (
    n.includes("\\google\\chrome\\user data\\optguideondevicemodel\\") &&
    n.endsWith("\\weights.bin")
  ) {
    return {
      id: "chrome-optguide-weights",
      label: "Модель Chrome (on-device AI)",
      severity: "caution",
    };
  }
  // Edge Provenance / ONNX models
  if (
    n.includes("\\microsoft\\edge\\user data\\provenancedata\\") &&
    (n.endsWith(".ort") || n.endsWith(".onnx") || n.endsWith("\\weights.bin"))
  ) {
    return {
      id: "edge-provenance-model",
      label: "Модель Edge (on-device AI)",
      severity: "caution",
    };
  }
  // Edge/Chrome generic on-device model folders
  if (
    (n.includes("\\google\\chrome\\user data\\") ||
      n.includes("\\microsoft\\edge\\user data\\")) &&
    (n.includes("\\optguideondevicemodel\\") ||
      n.includes("\\on_device_model\\") ||
      n.includes("\\optimization_guide_model_store\\")) &&
    (n.endsWith(".bin") || n.endsWith(".ort") || n.endsWith(".onnx") || n.endsWith(".pb"))
  ) {
    return {
      id: "browser-on-device-model",
      label: "Модель браузера (on-device)",
      severity: "caution",
    };
  }

  // ── caution: toolchain (зламає збірку, поки не перевстановити) ───
  if (
    n.includes("\\.rustup\\toolchains\\") &&
    (n.endsWith(".dll") || n.endsWith(".exe") || n.endsWith(".rlib"))
  ) {
    return {
      id: "rustup-toolchain",
      label: "Компонент Rust toolchain",
      severity: "caution",
    };
  }

  return null;
}
