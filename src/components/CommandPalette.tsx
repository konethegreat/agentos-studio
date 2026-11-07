import { Command, CornerDownLeft } from "lucide-react";
import { useEffect, useMemo, useRef, useState, type ReactNode } from "react";

export interface PaletteCommand {
  id: string;
  label: string;
  group: string;
  hint?: string;
  keys?: string;
  icon?: ReactNode;
  run: () => void;
}

/** Fuzzy-ish match: every query word must appear in the label, group or hint. */
function matches(c: PaletteCommand, q: string) {
  const hay = `${c.label} ${c.group} ${c.hint ?? ""}`.toLowerCase();
  return q
    .toLowerCase()
    .split(/\s+/)
    .filter(Boolean)
    .every((w) => hay.includes(w));
}

export function CommandPalette({ commands, open, onClose }: { commands: PaletteCommand[]; open: boolean; onClose: () => void }) {
  const [q, setQ] = useState("");
  const [idx, setIdx] = useState(0);
  const input = useRef<HTMLInputElement>(null);
  const list = useMemo(() => commands.filter((c) => matches(c, q)), [commands, q]);

  useEffect(() => {
    if (open) {
      setQ("");
      setIdx(0);
      window.setTimeout(() => input.current?.focus(), 10);
    }
  }, [open]);
  useEffect(() => { setIdx(0); }, [q]);

  if (!open) return null;

  function pick(c: PaletteCommand | undefined) {
    if (!c) return;
    onClose();
    window.setTimeout(c.run, 0);
  }

  let lastGroup = "";
  return (
    <div className="fixed inset-0 z-[60] flex items-start justify-center bg-black/50 pt-[12vh] backdrop-blur-[2px]" onMouseDown={onClose}>
      <div className="w-full max-w-xl overflow-hidden rounded-2xl border border-line bg-card shadow-2xl" onMouseDown={(e) => e.stopPropagation()} role="dialog" aria-label="Command palette">
        <div className="flex items-center gap-2 border-b border-line px-4">
          <Command className="size-4 text-slate-500" />
          <input
            ref={input}
            value={q}
            onChange={(e) => setQ(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Escape") onClose();
              if (e.key === "ArrowDown") {
                e.preventDefault();
                setIdx((i) => Math.min(i + 1, list.length - 1));
              }
              if (e.key === "ArrowUp") {
                e.preventDefault();
                setIdx((i) => Math.max(i - 1, 0));
              }
              if (e.key === "Enter") pick(list[idx]);
            }}
            placeholder="Type a command or search"
            aria-label="Command"
            aria-activedescendant={list[idx] ? `cmd-${list[idx].id}` : undefined}
            className="h-12 flex-1 bg-transparent text-sm text-slate-100 outline-none placeholder:text-slate-600"
          />
          <kbd className="rounded bg-white/5 px-1.5 py-0.5 text-[10px] text-slate-500">Esc</kbd>
        </div>
        <div className="max-h-[50vh] overflow-y-auto p-1.5" role="listbox">
          {list.length === 0 && <div className="px-3 py-6 text-center text-xs text-slate-600">No matching command</div>}
          {list.map((c, i) => {
            const header = c.group !== lastGroup ? c.group : null;
            lastGroup = c.group;
            return (
              <div key={c.id}>
                {header && <div className="px-3 pb-1 pt-2 text-[10px] font-semibold uppercase tracking-wider text-slate-600">{header}</div>}
                <button
                  id={`cmd-${c.id}`}
                  role="option"
                  aria-selected={i === idx}
                  onMouseEnter={() => setIdx(i)}
                  onClick={() => pick(c)}
                  className={`flex w-full items-center gap-3 rounded-lg px-3 py-2 text-left ${i === idx ? "bg-emerald-500/10" : ""}`}
                >
                  <span className="text-slate-400">{c.icon}</span>
                  <span className="min-w-0 flex-1">
                    <span className="block truncate text-[13px] text-slate-100">{c.label}</span>
                    {c.hint && <span className="block truncate text-[11px] text-slate-500">{c.hint}</span>}
                  </span>
                  {c.keys && <kbd className="rounded bg-white/5 px-1.5 py-0.5 text-[10px] text-slate-500">{c.keys}</kbd>}
                  {i === idx && <CornerDownLeft className="size-3.5 text-emerald-300" />}
                </button>
              </div>
            );
          })}
        </div>
      </div>
    </div>
  );
}
