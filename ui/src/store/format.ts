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

/**
 * Вік файла з ISO-дати останнього доступу: «сьогодні» / «N дн» / «N міс» /
 * «N р». Спільний для плитки (T-100) і накладки Live Preview (T-141).
 */
export function formatAge(isoDate: string): string {
  const accessed = Date.parse(isoDate);
  if (!Number.isFinite(accessed)) return "вік —";
  const days = Math.max(0, Math.floor((Date.now() - accessed) / 86_400_000));
  if (days < 1) return "сьогодні";
  if (days < 30) return `${days} дн`;
  const months = Math.floor(days / 30);
  if (months < 24) return `${months} міс`;
  return `${Math.floor(months / 12)} р`;
}
