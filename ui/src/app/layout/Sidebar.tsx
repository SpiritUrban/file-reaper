/**
 * Sidebar — список категорій-детекторів з обсягами (docs/ui.md §1).
 * T-105: живі обсяги (cleanup.total_updated / category.updated), сортування
 * за вагою, порожні категорії приглушені.
 * T-106: згортання до іконок (клік або `[`), живий бейдж Quarantine,
 * блок дисків зі смужками заповнення і перезапуском скану.
 */

import { useEffect, useState } from "react";
import { NavLink } from "react-router-dom";

import { AnimatedBytes, AnimatedInteger } from "@/components/AnimatedCounter";
import { Meter } from "@/components/Meter";
import { categoryRowsByWeight } from "@/store/categories";
import { toast } from "@/store/toasts";
import { useAppState } from "@/store/appState";
import { command, ipcErrorMessage } from "@/ipc/client";
import type { HotkeyActionEventDetail } from "@/hotkeys";
import type { ScanStartAck, VolumeUsageInfo } from "@/ipc/types";

const itemBase =
  "flex items-center gap-2 rounded px-2 py-1.5 text-sm hover:bg-panel-2";
const itemActive = "bg-panel-2 text-ink";
const itemIdle = "text-ink-dim";
/** Порожня категорія: приглушена, але лишається клікабельною. */
const itemEmpty = "text-ink-faint";
/** Диск «гарячий» — заповнення понад поріг підсвічує смужку heat-шкалою. */
const DISK_HOT_FRACTION = 0.85;

function usedFraction(volume: VolumeUsageInfo): number | null {
  if (volume.capacityBytes <= 0) return null;
  return (volume.capacityBytes - volume.freeBytes) / volume.capacityBytes;
}

/** Рескан одного тому: статус і рестарт скану — не навігація (ui.md §1). */
function rescanVolume(volume: string): void {
  void command<ScanStartAck>("scan.start", {
    payload: { volumes: [volume] },
  }).catch((error) =>
    toast({
      message: ipcErrorMessage(error),
      tone: "warning",
    }),
  );
}

export function Sidebar() {
  const { cleanup, scanRunning, volumes, quarantine } = useAppState();
  const [collapsed, setCollapsed] = useState(false);
  const rows = categoryRowsByWeight(cleanup.categories);
  const hasTotal = cleanup.reclaimableBytes > 0;
  const hasQuarantine = quarantine.heldCount > 0;

  // Хоткей `[` (T-103, дія toggle_sidebar) — той самий перемикач, що й клік.
  useEffect(() => {
    const onHotkey = (event: Event) => {
      const { action } = (event as CustomEvent<HotkeyActionEventDetail>).detail;
      if (action === "toggle_sidebar") setCollapsed((value) => !value);
    };
    window.addEventListener("trashradar:hotkey", onHotkey);
    return () => window.removeEventListener("trashradar:hotkey", onHotkey);
  }, []);

  return (
    <aside
      className={`flex ${collapsed ? "w-12" : "w-56"} shrink-0 flex-col border-r border-line bg-panel`}
      data-collapsed={collapsed}
    >
      {/* Логотип + перемикач згортання (клік або `[`) */}
      <div
        className={`flex items-center gap-2 py-3 ${collapsed ? "justify-center px-0" : "px-3"}`}
      >
        {!collapsed && (
          <>
            <span className="text-quarantine">◉</span>
            <span className="flex-1 font-semibold tracking-wide">
              TrashRadar
            </span>
          </>
        )}
        <button
          type="button"
          onClick={() => setCollapsed((value) => !value)}
          title={collapsed ? "Розгорнути Sidebar ([)" : "Згорнути Sidebar ([)"}
          aria-label={collapsed ? "Розгорнути Sidebar" : "Згорнути Sidebar"}
          aria-expanded={!collapsed}
          className="rounded px-1 text-xs text-ink-faint hover:bg-panel-2 hover:text-ink"
        >
          {collapsed ? "»" : "«"}
        </button>
      </div>

      {/* Головний екран: Cleanup з живою цифрою; ⟳ — активний скан */}
      <nav className="flex flex-col gap-0.5 px-2">
        <NavLink
          to="/"
          end
          title="Cleanup"
          className={({ isActive }) =>
            `${itemBase} ${collapsed ? "justify-center" : ""} ${isActive ? itemActive : itemIdle}`
          }
        >
          <span>▦</span>
          {!collapsed && (
            <>
              <span className="flex-1">Cleanup</span>
              {scanRunning ? (
                <span
                  className="inline-block animate-spin text-xs text-accent"
                  aria-label="сканування триває"
                >
                  ⟳
                </span>
              ) : null}
              {hasTotal ? (
                <AnimatedBytes
                  value={cleanup.reclaimableBytes}
                  className="font-mono text-xs text-ink-dim"
                />
              ) : (
                <span className="font-mono text-xs text-ink-faint">—</span>
              )}
            </>
          )}
        </NavLink>
      </nav>

      <div className="mx-3 my-2 border-t border-line" />

      {/* Категорії за вагою: найважча зверху, порожні приглушені (T-105) */}
      <nav className="flex flex-1 flex-col gap-0.5 overflow-y-auto px-2">
        {rows.map(({ descriptor, summary }) => {
          const totalBytes = summary?.totalBytes ?? 0;
          const isEmpty = totalBytes === 0;
          return (
            <NavLink
              key={descriptor.id}
              to={`/category/${descriptor.id}`}
              title={descriptor.title}
              className={({ isActive }) =>
                `${itemBase} ${collapsed ? "justify-center" : ""} ${isActive ? itemActive : isEmpty ? itemEmpty : itemIdle}`
              }
            >
              <span className="w-4 text-center">{descriptor.glyph}</span>
              {!collapsed && (
                <>
                  <span className="flex-1 truncate">{descriptor.title}</span>
                  {isEmpty ? (
                    <span className="font-mono text-xs text-ink-faint">—</span>
                  ) : (
                    <AnimatedBytes
                      value={totalBytes}
                      className="font-mono text-xs text-ink-dim"
                    />
                  )}
                </>
              )}
            </NavLink>
          );
        })}
      </nav>

      <div className="mx-3 my-2 border-t border-line" />

      {/* Quarantine з живим бейджем: кількість │ обсяг (T-106) */}
      <nav className="px-2">
        <NavLink
          to="/quarantine"
          title="Quarantine"
          className={({ isActive }) =>
            `${itemBase} ${collapsed ? "justify-center" : ""} ${isActive ? "bg-panel-2 text-quarantine" : "text-quarantine/80"}`
          }
        >
          <span>☣</span>
          {!collapsed && (
            <>
              <span className="flex-1">Quarantine</span>
              {hasQuarantine ? (
                <span className="font-mono text-xs">
                  <AnimatedInteger value={quarantine.heldCount} />
                  <span className="text-quarantine/60">│</span>
                  <AnimatedBytes value={quarantine.heldBytes} />
                </span>
              ) : (
                <span className="font-mono text-xs text-ink-faint">—</span>
              )}
            </>
          )}
        </NavLink>
      </nav>

      {/* Диски: живі смужки заповнення і рескан, НЕ навігація по вмісту */}
      {!collapsed && (
        <>
          <div className="mx-3 my-2 border-t border-line" />
          <div className="flex flex-col gap-2 px-4 pb-2">
            {volumes.length === 0 ? (
              <div className="flex items-center gap-2 text-xs text-ink-dim">
                <span className="font-mono">—</span>
                <Meter fraction={null} />
                <span className="font-mono text-ink-faint">—%</span>
              </div>
            ) : (
              volumes.map((volume) => {
                const fraction = usedFraction(volume);
                return (
                  <div
                    key={volume.volume}
                    className="flex items-center gap-2 text-xs text-ink-dim"
                  >
                    <span className="font-mono">{volume.volume}</span>
                    <Meter
                      fraction={fraction}
                      hot={fraction !== null && fraction >= DISK_HOT_FRACTION}
                    />
                    <span className="font-mono text-ink-faint">
                      {fraction === null ? "—%" : `${Math.round(fraction * 100)}%`}
                    </span>
                    <button
                      type="button"
                      onClick={() => rescanVolume(volume.volume)}
                      title={`Пересканувати ${volume.volume}`}
                      aria-label={`Пересканувати ${volume.volume}`}
                      className="rounded px-1 text-ink-faint hover:bg-panel-2 hover:text-ink"
                    >
                      ⟳
                    </button>
                  </div>
                );
              })
            )}
          </div>
        </>
      )}

      <div className="mx-3 my-1 border-t border-line" />

      <nav className="px-2 pb-2">
        <NavLink
          to="/health"
          title="Health"
          className={({ isActive }) =>
            `${itemBase} ${collapsed ? "justify-center" : ""} ${isActive ? itemActive : itemIdle}`
          }
        >
          <span>↯</span>
          {!collapsed && <span>Health</span>}
        </NavLink>
        <NavLink
          to="/settings"
          title="Налаштування"
          className={({ isActive }) =>
            `${itemBase} ${collapsed ? "justify-center" : ""} ${isActive ? itemActive : itemIdle}`
          }
        >
          <span>⛭</span>
          {!collapsed && <span>Налаштування</span>}
        </NavLink>
      </nav>
    </aside>
  );
}
