/**
 * Налаштування — одна сторінка, секції за docs/ui.md §9.
 * Каркас: секції-заголовки. Живі контроли — T-150…T-153.
 */

const SECTIONS: ReadonlyArray<{ title: string; taskRef: string }> = [
  { title: "Quarantine", taskRef: "T-150" },
  { title: "Сканування", taskRef: "T-151" },
  { title: "Категорії-детектори", taskRef: "T-152" },
  { title: "Керування", taskRef: "T-153" },
  { title: "Про програму", taskRef: "T-009" },
];

export function SettingsScreen() {
  return (
    <div className="flex flex-col gap-6 px-6 py-4">
      {SECTIONS.map((section) => (
        <section key={section.title}>
          <h2 className="text-xs font-semibold uppercase tracking-wider text-ink-dim">
            {section.title}
          </h2>
          <div className="mt-2 rounded border border-line bg-panel px-3 py-4 text-xs text-ink-faint">
            наповнюється задачею {section.taskRef} ·{" "}
            <span className="font-mono">docs/tasks.md</span>
          </div>
        </section>
      ))}
    </div>
  );
}
