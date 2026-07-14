/**
 * Налаштування — одна сторінка, секції за docs/ui.md §9 (T-150).
 *
 * Модель збереження (T-092/T-093): контроли редагують повний AppSettings і
 * шлють `settings.set`; Core валідує, атомарно пише файл, гаряче застосовує
 * (перерахунок детекторів / перепланування Quarantine) і шле
 * `settings.changed` — appState оновлюється, контроли передеруються без
 * ручного стейту. Пороги детекторів ідуть через `category.set_threshold`
 * (той самий шлях у Core) — спільний `ThresholdInput` з рядка детектора
 * категорії (§9.3: «дублює швидке налаштування»).
 *
 * Live Preview (дія кліку за замовч., затримка розрядження) — UI-стан у
 * livePreviewStore/localStorage (рішення T-139: геометрія і поведінка
 * webview — не Core settings.json).
 *
 * Заглушки наступних задач: редактор виключених шляхів — T-151; перемикачі
 * категорій і власні шаблони — T-152; редактор гарячих клавіш — T-153.
 */

import { useEffect, useState } from "react";
import { useNavigate } from "react-router-dom";

import { ThresholdInput } from "@/components/ThresholdInput";
import { DEFAULT_HOTKEYS } from "@/hotkeys";
import { command, ipcErrorMessage, isTauri } from "@/ipc/client";
import { ARMED_ACTIONS, type ArmedAction } from "@/store/armedAction";
import { useAppState } from "@/store/appState";
import { CATEGORIES, categoryRule } from "@/store/categories";
import { CATEGORY_THRESHOLDS, effectiveThreshold } from "@/store/detectorThresholds";
import { formatBytes } from "@/store/format";
import {
  livePreviewStore,
  MAX_DISARM_TIMEOUT_SEC,
  MIN_DISARM_TIMEOUT_SEC,
  useLivePreview,
} from "@/store/livePreview";
import { toast } from "@/store/toasts";
import type { AppSettings, CacheUsage, HealthInfo } from "@/ipc/types";

const MIB = 1024 * 1024;
const GIB = 1024 * MIB;

/** Варіанти автознищення Quarantine (ui.md §9.1; «ніколи» — див. відхилення
 *  T-150 у progress.md: Core вимагає TTL 1..3650 дн). */
const TTL_OPTIONS = [7, 30, 90, 365] as const;

/** settings.set з повним об'єктом; помилка валідації Core → toast з полем. */
async function saveSettings(next: AppSettings): Promise<void> {
  try {
    await command<AppSettings>("settings.set", { settings: next });
    // Стан оновиться подією settings.changed → appState (T-093/T-098).
  } catch (error) {
    toast({ message: ipcErrorMessage(error), tone: "warning" });
  }
}

/**
 * Числове поле «редагуй → blur/Enter → збережи» (патерн ThresholdInput).
 * Батько передає key={value}: зовнішня зміна (подія settings.changed)
 * перемонтовує поле й скидає чернетку на актуальне значення.
 */
function NumberField({
  value,
  suffix,
  min,
  max,
  onCommit,
}: {
  value: number;
  suffix: string;
  min: number;
  max?: number;
  onCommit: (next: number) => void | Promise<void>;
}) {
  const [draft, setDraft] = useState(String(value));
  const [saving, setSaving] = useState(false);

  const commit = async () => {
    const parsed = Math.round(Number(draft));
    if (
      !Number.isFinite(parsed) ||
      parsed < min ||
      (max !== undefined && parsed > max)
    ) {
      setDraft(String(value));
      return;
    }
    if (parsed === value) return;
    setSaving(true);
    try {
      await onCommit(parsed);
    } finally {
      setSaving(false);
    }
  };

  return (
    <span className="inline-flex items-center gap-1">
      <input
        type="number"
        min={min}
        {...(max !== undefined ? { max } : {})}
        value={draft}
        disabled={saving}
        onChange={(e) => setDraft(e.target.value)}
        onBlur={commit}
        onKeyDown={(e) => {
          if (e.key === "Enter") (e.target as HTMLInputElement).blur();
        }}
        className="w-20 border-b border-line bg-transparent px-1 text-right font-mono text-ink focus:border-accent focus:outline-none"
      />
      <span className="text-ink-dim">{suffix}</span>
    </span>
  );
}

/** Рядок «підпис: контрол» у секції. */
function Row({
  label,
  children,
}: {
  label: string;
  children: React.ReactNode;
}) {
  return (
    <div className="flex flex-wrap items-center gap-2 py-1.5">
      <span className="w-64 shrink-0 text-ink-dim">{label}</span>
      <span className="flex flex-wrap items-center gap-2">{children}</span>
    </div>
  );
}

function Section({
  title,
  children,
}: {
  title: string;
  children: React.ReactNode;
}) {
  return (
    <section>
      <h2 className="text-xs font-semibold uppercase tracking-wider text-ink-dim">
        {title}
      </h2>
      <div className="mt-2 rounded border border-line bg-panel px-3 py-2 text-xs">
        {children}
      </div>
    </section>
  );
}

/** Заглушка контрола, що належить наступній задачі беклогу. */
function PlannedButton({ label, taskRef }: { label: string; taskRef: string }) {
  return (
    <button
      type="button"
      disabled
      title={`Редактор — задача ${taskRef}`}
      className="cursor-not-allowed rounded border border-line bg-panel-2 px-2 py-0.5 text-ink-faint"
    >
      {label} · {taskRef}
    </button>
  );
}

const SELECT_CLASS =
  "rounded border border-line bg-panel-2 px-1.5 py-0.5 text-ink focus:border-accent focus:outline-none";

export function SettingsScreen() {
  const navigate = useNavigate();
  const { settings, volumes } = useAppState();
  const livePreview = useLivePreview();

  // «Про програму / діагностика»: версія з app.health, обсяг кешів з
  // cache.get_usage (DoD T-094: налаштування показують фактичний обсяг).
  const [health, setHealth] = useState<HealthInfo | null>(null);
  const [cacheUsage, setCacheUsage] = useState<CacheUsage | null>(null);
  const [clearingCache, setClearingCache] = useState(false);

  useEffect(() => {
    if (!isTauri()) return;
    command<HealthInfo>("app.health")
      .then(setHealth)
      .catch(() => setHealth(null));
    command<CacheUsage>("cache.get_usage")
      .then(setCacheUsage)
      .catch(() => setCacheUsage(null));
  }, []);

  const clearCache = async () => {
    setClearingCache(true);
    try {
      const cleared = await command<CacheUsage>("cache.clear");
      toast({
        message: `Кеш очищено: ${formatBytes(cleared.bytes)} · ${cleared.files} файлів.`,
        tone: "success",
      });
      setCacheUsage(await command<CacheUsage>("cache.get_usage"));
    } catch (error) {
      toast({ message: ipcErrorMessage(error), tone: "warning" });
    } finally {
      setClearingCache(false);
    }
  };

  const ttlDays = settings?.quarantine.ttlDays;
  const excluded = settings?.scan.excludedPaths ?? [];

  return (
    <div className="flex flex-col gap-6 px-6 py-4 text-xs">
      {settings === null ? (
        <p className="text-ink-faint">
          Налаштування Core недоступні (запустіть застосунок через Tauri).
        </p>
      ) : null}

      <Section title="Quarantine">
        <Row label="Сховище">
          <span className="font-mono text-ink-dim">
            {volumes.length > 0
              ? volumes
                  .map((volume) => `${volume.volume}\\.trashradar\\quarantine`)
                  .join("  ·  ")
              : "<том>:\\.trashradar\\quarantine"}
          </span>
          <span className="text-ink-faint">
            — на кожному записуваному томі; переміщення без копіювання
          </span>
        </Row>
        <Row label="Автознищення">
          {settings ? (
            <select
              className={SELECT_CLASS}
              value={ttlDays}
              onChange={(e) =>
                void saveSettings({
                  ...settings,
                  quarantine: {
                    ...settings.quarantine,
                    ttlDays: Number(e.target.value),
                  },
                })
              }
            >
              {TTL_OPTIONS.map((days) => (
                <option key={days} value={days}>
                  {days} дн
                </option>
              ))}
              {ttlDays !== undefined &&
              !TTL_OPTIONS.some((days) => days === ttlDays) ? (
                <option value={ttlDays}>{ttlDays} дн (поточне)</option>
              ) : null}
            </select>
          ) : (
            <span className="text-ink-faint">—</span>
          )}
        </Row>
        <Row label="Попереджати при обсязі понад">
          {settings ? (
            <NumberField
              key={settings.quarantine.warningThresholdBytes}
              value={Math.max(
                1,
                Math.round(settings.quarantine.warningThresholdBytes / GIB),
              )}
              suffix="ГБ"
              min={1}
              onCommit={(gib) =>
                saveSettings({
                  ...settings,
                  quarantine: {
                    ...settings.quarantine,
                    warningThresholdBytes: gib * GIB,
                  },
                })
              }
            />
          ) : (
            <span className="text-ink-faint">—</span>
          )}
        </Row>
      </Section>

      <Section title="Сканування">
        <Row label="Мін. розмір файла для радара">
          {settings ? (
            <NumberField
              key={settings.scan.minimumSizeBytes}
              value={Math.round(settings.scan.minimumSizeBytes / MIB)}
              suffix="МБ (0 — без порога)"
              min={0}
              onCommit={(mib) =>
                saveSettings({
                  ...settings,
                  scan: { ...settings.scan, minimumSizeBytes: mib * MIB },
                })
              }
            />
          ) : (
            <span className="text-ink-faint">—</span>
          )}
        </Row>
        <Row label="Виключені шляхи">
          <span className="text-ink-dim">
            {excluded.length > 0 ? (
              <span className="font-mono">
                {excluded.slice(0, 4).join(", ")}
                {excluded.length > 4 ? ` … (+${excluded.length - 4})` : ""}
              </span>
            ) : (
              "власних немає"
            )}
          </span>
          <span className="text-ink-faint">
            · системні (Windows, Program Files, $Recycle.Bin, …) виключені
            завжди
          </span>
          <PlannedButton label="Змінити" taskRef="T-151" />
        </Row>
      </Section>

      <Section title="Категорії-детектори">
        <div className="flex flex-col divide-y divide-line/60">
          {CATEGORIES.map((category) => {
            const fields = CATEGORY_THRESHOLDS[category.id] ?? [];
            return (
              <div
                key={category.id}
                className="flex flex-wrap items-center gap-2 py-1.5"
              >
                <span className="w-64 shrink-0">
                  <span className="mr-1.5 text-ink-faint">{category.glyph}</span>
                  {category.title}
                </span>
                <span className="flex-1 text-ink-faint">
                  {categoryRule(category.id)}
                </span>
                <span className="flex items-center gap-3">
                  {fields.map((field) => (
                    <ThresholdInput
                      key={`${field.key}:${effectiveThreshold(settings, category.id, field)}`}
                      categoryId={category.id}
                      field={field}
                      value={effectiveThreshold(settings, category.id, field)}
                    />
                  ))}
                </span>
              </div>
            );
          })}
        </div>
        <div className="flex items-center gap-2 pt-2 text-ink-faint">
          Перемикачі категорій і власні шаблони папок —
          <PlannedButton label="Налаштувати" taskRef="T-152" />
        </div>
      </Section>

      <Section title="Керування">
        <Row label="Гарячі клавіші">
          <span className="text-ink-dim">
            комбінацій: {DEFAULT_HOTKEYS.length} (зведення ui.md §11)
          </span>
          <PlannedButton label="Переглянути" taskRef="T-153" />
        </Row>
        <Row label="Live Preview: дія кліку за замовч.">
          <select
            className={SELECT_CLASS}
            value={livePreview.defaultArmedAction}
            onChange={(e) =>
              livePreviewStore.setDefaultArmedAction(
                e.target.value as ArmedAction,
              )
            }
          >
            {ARMED_ACTIONS.map((meta) => (
              <option key={meta.id} value={meta.id}>
                {meta.label}
              </option>
            ))}
          </select>
        </Row>
        <Row label="Авторозрядження деструктивної дії">
          <NumberField
            key={livePreview.disarmTimeoutSec}
            value={livePreview.disarmTimeoutSec}
            suffix={`с (${MIN_DISARM_TIMEOUT_SEC}–${MAX_DISARM_TIMEOUT_SEC})`}
            min={MIN_DISARM_TIMEOUT_SEC}
            max={MAX_DISARM_TIMEOUT_SEC}
            onCommit={(seconds) => livePreviewStore.setDisarmTimeoutSec(seconds)}
          />
        </Row>
      </Section>

      <Section title="Про програму / діагностика">
        <Row label="Версія">
          <span className="font-mono text-ink-dim">
            {health ? `TrashRadar ${health.appVersion}` : "—"}
          </span>
          {health ? (
            <span className="text-ink-faint">
              · Core: {health.coreStatus} ·{" "}
              {health.elevated ? "адмін-права" : "без адмін-прав"}
            </span>
          ) : null}
        </Row>
        <Row label="Кеші TrashRadar">
          <span className="font-mono text-ink-dim">
            {cacheUsage
              ? `${formatBytes(cacheUsage.bytes)} · ${cacheUsage.files} файлів`
              : "—"}
          </span>
          <button
            type="button"
            disabled={clearingCache || !isTauri()}
            onClick={() => void clearCache()}
            className="rounded border border-line bg-panel-2 px-2 py-0.5 text-ink hover:border-accent disabled:cursor-not-allowed disabled:text-ink-faint"
          >
            Очистити кеш
          </button>
        </Row>
        <Row label="Діагностика">
          <button
            type="button"
            onClick={() => navigate("/health")}
            className="rounded border border-line bg-panel-2 px-2 py-0.5 text-ink hover:border-accent"
          >
            Health-екран
          </button>
          <span className="text-ink-faint">
            модулі Core, IPC-лічильники, стратегії скану
          </span>
        </Row>
      </Section>
    </div>
  );
}
