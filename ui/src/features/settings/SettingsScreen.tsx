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
 * Виключені шляхи (T-151): редактор списку в секції «Сканування»; системні
 * виключення T-027 видимі; сканер читає список на старті сесії — зміни
 * діють з наступного скану.
 *
 * Перемикачі детекторів (T-152): чекбокс на категоріях живої ферми скану;
 * вимкнення йде через `settings.set` → Core перераховує індекс і шле свіжі
 * totals — категорія зникає з Sidebar/Summary/цифри миттєво, без рескану.
 *
 * Гарячі клавіші (T-153): [Переглянути] розгортає HotkeysEditor — живий стан
 * реєстру T-103, перепризначення з перевіркою конфліктів, localStorage.
 */

import { useEffect, useState } from "react";
import { useNavigate } from "react-router-dom";

import { ThresholdInput } from "@/components/ThresholdInput";
import { FfmpegRow } from "@/features/settings/FfmpegRow";
import { hotkeys } from "@/hotkeys";
import { command, ipcErrorMessage, isTauri } from "@/ipc/client";
import { openExternal } from "@/ipc/external";
import { PRODUCT_METADATA } from "@/product";
import { ARMED_ACTIONS, type ArmedAction } from "@/store/armedAction";
import { appStateStore, useAppState } from "@/store/appState";
import {
  CATEGORIES,
  categoryRule,
  isCategoryEnabled,
  TOGGLEABLE_CATEGORIES,
} from "@/store/categories";
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

import { HotkeysEditor } from "./HotkeysEditor";

const MIB = 1024 * 1024;
const GIB = 1024 * MIB;

/**
 * Системні виключення обходу (T-027, дзеркало
 * `PathExclusions::windows_system_defaults` у core/infra/scan-walk) +
 * службовий каталог карантину (T-088). Застосовуються Core-ом до кожного
 * тому завжди; тут — лише видимість (DoD T-151).
 */
const SYSTEM_EXCLUSIONS = [
  "Windows",
  "Program Files",
  "Program Files (x86)",
  "ProgramData",
  "$Recycle.Bin",
  "System Volume Information",
  "Recovery",
  "Config.Msi",
  ".trashradar",
] as const;

/** Варіанти автознищення Quarantine (ui.md §9.1; «ніколи» — див. відхилення
 *  T-150 у progress.md: Core вимагає TTL 1..3650 дн). */
const TTL_OPTIONS = [7, 30, 90, 365] as const;

/** settings.set з повним об'єктом; помилка валідації Core → toast + rollback. */
async function saveSettings(next: AppSettings): Promise<void> {
  const previous = appStateStore.getSnapshot().settings;
  // Оптимістично — чекбокси/поля реагують одразу (Core раніше блокував
  // IPC на повний перерахунок індексу → «не перемикаються»).
  appStateStore.patchSettings(next);
  try {
    await command<AppSettings>("settings.set", { settings: next });
    // Підтвердження / узгодження: settings.changed → appState.
  } catch (error) {
    if (previous) appStateStore.patchSettings(previous);
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

const SELECT_CLASS =
  "rounded border border-line bg-panel-2 px-1.5 py-0.5 text-ink focus:border-accent focus:outline-none";

/**
 * Редактор виключених шляхів (T-151, ui.md §9.2): видимі системні виключення
 * + користувацький список з додаванням/видаленням. Список зберігається через
 * `settings.set` (валідація Core: непорожні елементи, ліміт 4096); сканер
 * читає його на старті сесії — зміни діють з наступного скану.
 */
function ExcludedPathsEditor({ settings }: { settings: AppSettings }) {
  const [draft, setDraft] = useState("");
  const excluded = settings.scan.excludedPaths;

  const save = (excludedPaths: string[]) =>
    saveSettings({
      ...settings,
      scan: { ...settings.scan, excludedPaths },
    });

  const addPath = async () => {
    const path = draft.trim();
    if (path.length === 0) return;
    const exists = excluded.some(
      (item) => item.toLowerCase() === path.toLowerCase(),
    );
    if (exists) {
      toast({ message: "Цей шлях уже у списку виключень.", tone: "info" });
      return;
    }
    await save([...excluded, path]);
    setDraft("");
  };

  const removePath = (index: number) =>
    void save(excluded.filter((_, i) => i !== index));

  return (
    <div className="flex flex-col gap-2">
      {excluded.length > 0 ? (
        <ul className="flex flex-col gap-1">
          {excluded.map((path, index) => (
            <li key={`${path}:${index}`} className="flex items-center gap-2">
              <button
                type="button"
                aria-label={`Видалити виключення ${path}`}
                title="Видалити зі списку"
                onClick={() => removePath(index)}
                className="rounded border border-line bg-panel-2 px-1.5 text-ink-dim hover:border-accent hover:text-ink"
              >
                ✕
              </button>
              <span className="font-mono text-ink-dim">{path}</span>
            </li>
          ))}
        </ul>
      ) : (
        <div className="text-ink-faint">Власних виключень немає.</div>
      )}

      <div className="flex items-center gap-2">
        <input
          type="text"
          value={draft}
          placeholder="D:\Projects\archive"
          onChange={(e) => setDraft(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") void addPath();
          }}
          className="w-80 border-b border-line bg-transparent px-1 font-mono text-ink placeholder:text-ink-faint focus:border-accent focus:outline-none"
        />
        <button
          type="button"
          disabled={draft.trim().length === 0}
          onClick={() => void addPath()}
          className="rounded border border-line bg-panel-2 px-2 py-0.5 text-ink hover:border-accent disabled:cursor-not-allowed disabled:text-ink-faint"
        >
          Додати
        </button>
        <span className="text-ink-faint">діє з наступного скану</span>
      </div>
    </div>
  );
}

export function SettingsScreen() {
  const navigate = useNavigate();
  const { settings, volumes } = useAppState();
  const livePreview = useLivePreview();

  // «Про програму / діагностика»: версія з app.health, обсяг кешів з
  // cache.get_usage (DoD T-094: налаштування показують фактичний обсяг).
  const [health, setHealth] = useState<HealthInfo | null>(null);
  const [cacheUsage, setCacheUsage] = useState<CacheUsage | null>(null);
  const [clearingCache, setClearingCache] = useState(false);
  // T-153: таблиця гарячих клавіш згорнута за замовчуванням (ui.md §9 wireframe:
  // «Гарячі клавіші … [Переглянути]»).
  const [showHotkeys, setShowHotkeys] = useState(false);

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
        <Row label="«Майже порожні»: макс. файлів у папці">
          {settings ? (
            <NumberField
              key={settings.scan.sparseMaxFiles}
              value={settings.scan.sparseMaxFiles}
              suffix="файлів (1–1000)"
              min={1}
              max={1000}
              onCommit={(n) =>
                saveSettings({
                  ...settings,
                  scan: { ...settings.scan, sparseMaxFiles: n },
                })
              }
            />
          ) : (
            <span className="text-ink-faint">—</span>
          )}
        </Row>
        <Row label="«Глибокі шляхи»: макс. глибина вкладення">
          {settings ? (
            <NumberField
              key={settings.scan.deepPathMaxDepth}
              value={settings.scan.deepPathMaxDepth}
              suffix="рівнів (2–64)"
              min={2}
              max={64}
              onCommit={(n) =>
                saveSettings({
                  ...settings,
                  scan: { ...settings.scan, deepPathMaxDepth: n },
                })
              }
            />
          ) : (
            <span className="text-ink-faint">—</span>
          )}
        </Row>
        <div className="pt-1 text-ink-faint">
          Пороги папок діють з наступного скану.
        </div>
        <div className="flex flex-wrap gap-2 py-1.5">
          <span className="w-64 shrink-0 text-ink-dim">Диски в аналізі</span>
          <div className="flex min-w-0 flex-1 flex-col gap-1">
            {settings && volumes.length > 0 ? (
              volumes.map((v) => {
                const letter = v.volume.replace(/[^A-Za-z]/g, "").toUpperCase();
                const excluded = (settings.scan.excludedVolumes ?? []).some(
                  (x) => x.toUpperCase() === letter,
                );
                const usedBytes = Math.max(0, v.capacityBytes - v.freeBytes);
                return (
                  <label
                    key={v.volume}
                    className={`flex cursor-pointer items-center gap-2 ${excluded ? "text-ink-faint" : ""}`}
                    title={
                      excluded
                        ? "Увімкнути диск в аналіз"
                        : "Вимкнути диск з аналізу (не сканувати)"
                    }
                  >
                    <input
                      type="checkbox"
                      checked={!excluded}
                      onChange={(e) => {
                        const current = settings.scan.excludedVolumes ?? [];
                        const next = e.target.checked
                          ? current.filter((x) => x.toUpperCase() !== letter)
                          : [...current, letter];
                        void saveSettings({
                          ...settings,
                          scan: { ...settings.scan, excludedVolumes: next },
                        });
                      }}
                      className="accent-[--color-accent]"
                    />
                    <span className="font-mono text-ink">{v.volume}</span>
                    <span className="text-ink-faint">
                      {formatBytes(usedBytes)} / {formatBytes(v.capacityBytes)}{" "}
                      зайнято
                    </span>
                  </label>
                );
              })
            ) : (
              <span className="text-ink-faint">Список томів недоступний.</span>
            )}
            <span className="text-ink-faint">
              Знята галочка — диск не сканується; діє з наступного скану.
            </span>
          </div>
        </div>
        <div className="flex flex-wrap gap-2 py-1.5">
          <span className="w-64 shrink-0 text-ink-dim">Виключені шляхи</span>
          <div className="flex min-w-0 flex-1 flex-col gap-2">
            {/* DoD T-151: системні виключення (T-027 + T-088) видимі завжди. */}
            <div className="text-ink-faint">
              Системні (завжди, на кожному томі):{" "}
              <span className="font-mono">
                {SYSTEM_EXCLUSIONS.map((name) => `<том>:\\${name}`).join(", ")}
              </span>
            </div>
            {settings ? (
              <ExcludedPathsEditor settings={settings} />
            ) : (
              <span className="text-ink-faint">—</span>
            )}
          </div>
        </div>
      </Section>

      <Section title="Категорії-детектори">
        <div className="flex flex-col divide-y divide-line/60">
          {CATEGORIES.map((category) => {
            const fields = CATEGORY_THRESHOLDS[category.id] ?? [];
            const toggleable = TOGGLEABLE_CATEGORIES.includes(category.id);
            const enabled = isCategoryEnabled(settings, category.id);
            return (
              <div
                key={category.id}
                className="flex flex-wrap items-center gap-2 py-1.5"
              >
                {/* T-152: перемикач — лише детектори живої ферми скану. */}
                <label
                  className={`flex w-64 shrink-0 items-center gap-2 ${
                    toggleable && settings ? "cursor-pointer" : ""
                  } ${enabled ? "" : "text-ink-faint"}`}
                  title={
                    toggleable
                      ? enabled
                        ? "Вимкнути детектор: категорія зникне з Sidebar і цифри"
                        : "Увімкнути детектор"
                      : "Детектор без перемикача в MVP (каскад дублікатів / ще не підключений до скану)"
                  }
                >
                  <input
                    type="checkbox"
                    checked={enabled}
                    disabled={!toggleable || settings === null}
                    onChange={(e) => {
                      if (!settings) return;
                      void saveSettings({
                        ...settings,
                        detectors: {
                          ...settings.detectors,
                          [category.id]: {
                            thresholds:
                              settings.detectors?.[category.id]?.thresholds ??
                              {},
                            enabled: e.target.checked,
                          },
                        },
                      });
                    }}
                    className="accent-[--color-accent]"
                  />
                  <span className="text-ink-faint">{category.glyph}</span>
                  {category.title}
                </label>
                <span className="flex-1 text-ink-faint">
                  {categoryRule(category.id)}
                </span>
                <span className="flex items-center gap-3">
                  {enabled
                    ? fields.map((field) => (
                        <ThresholdInput
                          key={`${field.key}:${effectiveThreshold(settings, category.id, field)}`}
                          categoryId={category.id}
                          field={field}
                          value={effectiveThreshold(settings, category.id, field)}
                        />
                      ))
                    : null}
                </span>
              </div>
            );
          })}
        </div>
        <div className="pt-2 text-ink-faint">
          Вимкнення діє миттєво без рескану; власні шаблони папок для
          dev-артефактів — поза MVP (реєстр{" "}
          <span className="font-mono">registry/dev-artifacts.json</span>).
        </div>
      </Section>

      <Section title="Керування">
        <Row label="Гарячі клавіші">
          <span className="text-ink-dim">
            комбінацій: {hotkeys.list().length} (зведення ui.md §11)
          </span>
          <button
            type="button"
            onClick={() => setShowHotkeys((current) => !current)}
            className="rounded border border-line bg-panel-2 px-2 py-0.5 text-ink hover:border-accent"
          >
            {showHotkeys ? "Згорнути" : "Переглянути"}
          </button>
        </Row>
        {showHotkeys ? (
          <div className="py-1.5 pl-2">
            <HotkeysEditor />
          </div>
        ) : null}
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
        {/* Правило 29: дія замість пасивного попередження. */}
        <Row label="Відео-превʼю (ffmpeg)">
          <FfmpegRow />
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
        {/* Розділ 7: Settings → About — поверхня, куди людина приходить
            сама, тож ім'я автора з посиланням на хаб тут доречне. */}
        <Row label="Автор">
          <span className="text-ink-dim">{PRODUCT_METADATA.author}</span>
          <button
            type="button"
            onClick={() => openExternal(PRODUCT_METADATA.authorUrl)}
            className="rounded border border-line bg-panel-2 px-2 py-0.5 text-ink hover:border-accent"
          >
            Інші проєкти та послуги ↗
          </button>
          <button
            type="button"
            onClick={() => openExternal(PRODUCT_METADATA.repositoryUrl)}
            className="rounded border border-line bg-panel-2 px-2 py-0.5 text-ink hover:border-accent"
          >
            Вихідний код ↗
          </button>
        </Row>
        <Row label="Ліцензія">
          <span className="text-ink-faint">
            {PRODUCT_METADATA.license} · {PRODUCT_METADATA.copyright}
          </span>
        </Row>
      </Section>
    </div>
  );
}
