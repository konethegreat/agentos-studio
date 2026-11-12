import { Bell, BellOff, CircleAlert, CircleCheck, CircleHelp, CircleX, Eye } from "lucide-react";
import { useEffect, useRef, useState, type ReactNode } from "react";
import { api, type Notice } from "../lib/api";

const ICON: Record<string, ReactNode> = {
  question: <CircleHelp className="size-4 text-amber-300" />,
  goal_plan: <CircleHelp className="size-4 text-sky-300" />,
  goal_done: <CircleCheck className="size-4 text-emerald-300" />,
  goal_failed: <CircleX className="size-4 text-red-300" />,
  mission_blocked: <CircleAlert className="size-4 text-amber-300" />,
  mission_failed: <CircleX className="size-4 text-red-300" />,
  review: <Eye className="size-4 text-sky-300" />,
  connection: <CircleAlert className="size-4 text-red-300" />,
};

function ago(at: number): string {
  const s = Math.max(0, Math.floor(Date.now() / 1000) - at);
  if (s < 60) return "just now";
  if (s < 3600) return `${Math.floor(s / 60)} min ago`;
  if (s < 86400) return `${Math.floor(s / 3600)} h ago`;
  return `${Math.floor(s / 86400)} d ago`;
}

/**
 * The notification centre: one bell with everything that needs attention, newest first, each
 * item opening the screen that owns it. Opening the panel marks everything read.
 */
export function NoticeBell({ onGo }: { onGo: (view: string) => void }) {
  const [items, setItems] = useState<Notice[]>([]);
  const [open, setOpen] = useState(false);
  const [osOn, setOsOn] = useState(true);
  const box = useRef<HTMLDivElement>(null);

  useEffect(() => {
    let stop = false;
    const load = () =>
      api
        .notices()
        .then((n) => {
          if (!stop) setItems(n);
        })
        .catch(() => undefined);
    void load();
    void api.getPref("notices.os").then((v) => setOsOn(v !== "off")).catch(() => undefined);
    const t = window.setInterval(() => void load(), 20_000);
    return () => {
      stop = true;
      window.clearInterval(t);
    };
  }, []);

  useEffect(() => {
    if (!open) return;
    const close = (e: MouseEvent) => {
      if (box.current && !box.current.contains(e.target as Node)) setOpen(false);
    };
    window.addEventListener("mousedown", close);
    return () => window.removeEventListener("mousedown", close);
  }, [open]);

  const unread = items.filter((n) => n.unread).length;

  function toggle() {
    const next = !open;
    setOpen(next);
    if (next && unread > 0) {
      void api.noticesRead().then(() => setItems((xs) => xs.map((x) => ({ ...x, unread: false }))));
    }
  }

  async function switchOs() {
    const next = !osOn;
    setOsOn(next);
    await api.setPref("notices.os", next ? "on" : "off");
  }

  return (
    <div className="relative" ref={box}>
      <button
        type="button"
        onClick={toggle}
        aria-label={unread ? `Notifications, ${unread} unread` : "Notifications"}
        aria-expanded={open}
        title={unread ? `${unread} unread notification${unread === 1 ? "" : "s"}` : "Notifications"}
        className={`relative inline-flex h-8 w-8 items-center justify-center rounded-lg ring-1 transition-colors ${
          open ? "bg-white/[0.06] text-slate-100 ring-line" : "text-slate-300 ring-line hover:bg-white/[0.04]"
        }`}
      >
        <Bell className="size-4" />
        {unread > 0 && (
          <span aria-hidden="true" className="absolute -right-1 -top-1 rounded-full bg-emerald-500 px-1.5 text-2xs font-bold leading-4 text-emerald-950">
            {unread > 99 ? "99+" : unread}
          </span>
        )}
      </button>
      {open && (
        <div className="absolute right-0 top-10 z-50 w-96 overflow-hidden rounded-xl border border-line bg-panel shadow-2xl">
          <div className="flex items-center justify-between border-b border-line px-4 py-2.5">
            <div className="text-sm font-semibold text-slate-100">Notifications</div>
            <button
              type="button"
              onClick={() => void switchOs()}
              title={osOn ? "Windows notifications are on for questions, finished goals and blocked missions. Click to turn them off." : "Windows notifications are off. Click to turn them on."}
              className="inline-flex items-center gap-1.5 rounded-md px-2 py-1 text-label text-muted hover:bg-white/[0.05] hover:text-slate-200"
            >
              {osOn ? <Bell className="size-3.5" /> : <BellOff className="size-3.5" />}
              {osOn ? "Windows alerts on" : "Windows alerts off"}
            </button>
          </div>
          <div className="max-h-[60vh] overflow-y-auto">
            {items.length === 0 ? (
              <div className="px-4 py-8 text-center text-small text-muted">Nothing needs you. Questions, finished goals and blocked missions appear here.</div>
            ) : (
              items.map((n) => (
                <button
                  key={n.id}
                  type="button"
                  onClick={() => {
                    setOpen(false);
                    onGo(n.view);
                  }}
                  className={`flex w-full gap-3 border-b border-line px-4 py-3 text-left transition-colors last:border-0 hover:bg-white/[0.04] ${n.unread ? "bg-emerald-500/[0.04]" : ""}`}
                >
                  <span className="mt-0.5 shrink-0">{ICON[n.kind] ?? <Bell className="size-4" />}</span>
                  <span className="min-w-0 flex-1">
                    <span className="flex items-baseline justify-between gap-2">
                      <span className="truncate text-sm font-medium text-slate-100">{n.title}</span>
                      <span className="shrink-0 text-2xs text-muted">{ago(n.at)}</span>
                    </span>
                    <span className="mt-0.5 line-clamp-2 block text-label text-muted">{n.detail}</span>
                    {n.open && <span className="mt-1 inline-block rounded-full bg-amber-500/10 px-1.5 text-2xs text-amber-300 ring-1 ring-amber-500/30">needs you</span>}
                  </span>
                </button>
              ))
            )}
          </div>
        </div>
      )}
    </div>
  );
}
