import { useEffect, useState, type ReactNode } from "react";
import { ChevronLeft, Pin, PinOff } from "lucide-react";
import { api } from "../lib/api";

export type RailView =
  | "today"
  | "chat"
  | "goals"
  | "projects"
  | "mission"
  | "fleet"
  | "agents"
  | "timeline"
  | "computer"
  | "memory"
  | "context"
  | "skills"
  | "capabilities"
  | "constitution"
  | "github"
  | "control"
  | "updates"
  | "safety"
  | "setup"
  | "diagnostics"
  | "help";

export type RailGroup = {
  id: string;
  label: string;
  icon: ReactNode;
  defaultView: RailView;
  tabs: { id: RailView; label: string; hint: string }[];
  badge?: number;
  /** What the badge counts, said in the tooltip and to screen readers. */
  badgeLabel?: string;
  /** The tab the count belongs to; it carries the same badge when the group is open. */
  badgeTab?: RailView;
};

/**
 * Collapsible navigation rail. 56 px collapsed, 208 px on hover or when pinned, with the pin
 * persisted in studio.db so a pinned rail survives a restart.
 */
export function NavRail({
  groups,
  view,
  onGo,
}: {
  groups: RailGroup[];
  view: RailView;
  onGo: (v: RailView) => void;
}) {
  const [pinned, setPinned] = useState(false);
  const [hover, setHover] = useState(false);
  const expanded = pinned || hover;
  const activeGroup = groups.find((g) => g.tabs.some((t) => t.id === view))?.id ?? "chat";

  useEffect(() => {
    void api
      .getPref("nav.pinned")
      .then((v) => setPinned(v === "1"))
      .catch(() => undefined);
  }, []);

  function togglePin() {
    const next = !pinned;
    setPinned(next);
    void api.setPref("nav.pinned", next ? "1" : "0").catch(() => undefined);
  }

  return (
    <nav
      aria-label="Main navigation"
      onMouseEnter={() => setHover(true)}
      onMouseLeave={() => setHover(false)}
      className={`relative z-20 flex shrink-0 flex-col border-r border-line bg-sunken transition-[width] duration-150 ${
        expanded ? "w-52" : "w-14"
      }`}
    >
      <div className="flex h-12 items-center gap-2 px-3.5">
        <span className="grid size-7 shrink-0 place-items-center rounded-lg bg-gradient-to-br from-emerald-400/30 to-sky-500/20 text-label font-bold text-emerald-200 ring-1 ring-emerald-400/30">
          AO
        </span>
        {expanded && <span className="truncate text-body-sm font-semibold text-slate-200">AgentOS</span>}
      </div>

      <div className="flex min-h-0 flex-1 flex-col gap-1 overflow-y-auto px-2 pb-2">
        {groups.map((g) => {
          const active = g.id === activeGroup;
          return (
            <div key={g.id}>
              <button
                type="button"
                onClick={() => onGo(g.defaultView)}
                aria-label={g.badge ? `${g.label}, ${g.badge} ${g.badgeLabel ?? "new"}` : g.label}
                title={`${g.label} (${g.tabs.map((t) => t.label).join(", ")})${
                  g.badge ? `\n${g.badge} ${g.badgeLabel ?? "new"}` : ""
                }`}
                aria-current={active ? "page" : undefined}
                className={`relative flex h-10 w-full items-center gap-3 rounded-lg px-3 text-left transition-colors ${
                  active ? "bg-white/[0.07] text-slate-100" : "text-slate-400 hover:bg-white/[0.04] hover:text-slate-200"
                }`}
              >
                <span className={`shrink-0 ${active ? "text-emerald-300" : ""}`}>{g.icon}</span>
                {expanded && <span className="truncate text-body-sm font-medium">{g.label}</span>}
                {g.badge ? (
                  <span
                    aria-hidden="true"
                    className={`${expanded ? "ml-auto" : "absolute right-1.5 top-1.5"} rounded-full bg-emerald-500 px-1.5 text-2xs font-bold leading-4 text-emerald-950`}
                  >
                    {g.badge > 99 ? "99+" : g.badge}
                  </span>
                ) : null}
              </button>
              {expanded && active && (
                <div className="mb-1 ml-4 mt-0.5 space-y-0.5 border-l border-line pl-2">
                  {g.tabs.map((t) => (
                    <button
                      key={t.id}
                      type="button"
                      onClick={() => onGo(t.id)}
                      title={g.badge && g.badgeTab === t.id ? `${t.hint}\n${g.badge} ${g.badgeLabel ?? "new"}` : t.hint}
                      className={`flex h-8 w-full items-center gap-2 rounded-md px-2 text-left text-ui transition-colors ${
                        view === t.id ? "bg-emerald-500/10 text-emerald-200" : "text-slate-400 hover:bg-white/[0.05] hover:text-slate-200"
                      }`}
                    >
                      <span className="truncate">{t.label}</span>
                      {g.badge && g.badgeTab === t.id ? (
                        <span
                          aria-label={`${g.badge} ${g.badgeLabel ?? "new"}`}
                          className="ml-auto rounded-full bg-emerald-500 px-1.5 text-2xs font-bold leading-4 text-emerald-950"
                        >
                          {g.badge > 99 ? "99+" : g.badge}
                        </span>
                      ) : null}
                    </button>
                  ))}
                </div>
              )}
            </div>
          );
        })}
      </div>

      {expanded && (
        <div className="border-t border-line px-3.5 py-2 text-2xs leading-tight text-slate-600">
          <div className="font-medium text-slate-500">K-ONE IT SOLUTIONS</div>
          <div>by Kone Tshivhinda</div>
        </div>
      )}
      <button
        type="button"
        onClick={togglePin}
        title={pinned ? "Unpin the rail (collapse on mouse out)" : "Pin the rail open"}
        aria-pressed={pinned}
        className="flex h-10 items-center gap-3 border-t border-line px-3.5 text-slate-500 transition-colors hover:text-slate-200"
      >
        {pinned ? <Pin className="size-4 shrink-0" /> : <PinOff className="size-4 shrink-0" />}
        {expanded ? <span className="text-small">{pinned ? "Pinned" : "Pin rail"}</span> : null}
        {expanded && <ChevronLeft className={`ml-auto size-3.5 transition-transform ${pinned ? "" : "rotate-180"}`} />}
      </button>
    </nav>
  );
}
