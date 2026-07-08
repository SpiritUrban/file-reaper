/** Форматування розмірів для UI. Розміри — завжди моноширинним шрифтом. */

const UNITS = ["Б", "КБ", "МБ", "ГБ", "ТБ"] as const;

export function formatBytes(bytes: number | null): string {
  if (bytes === null) return "—";
  if (bytes < 1024) return `${bytes} Б`;
  let value = bytes;
  let unit = 0;
  while (value >= 1024 && unit < UNITS.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return `${value >= 100 ? Math.round(value) : value.toFixed(1)} ${UNITS[unit]}`;
}
