import { Bell, BellOff, CircleAlert, CircleCheck, CircleHelp, CircleX, Eye } from "lucide-react";
import { useEffect, useId, useRef, useState, type ReactNode } from "react";
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

export function NoticeBell({ onGo }: { onGo: (view: string, notice: Notice) => void }) {
  const [items, setItems] = useState<Notice[] | null>(null);
  const [open, setOpen] = useState(false);
  const [osOn, setOsOn] = useState<boolean | null>(null);
  const [loadError, setLoadError] = useState(false);
  const [readError, setReadError] = useState(false);
  const [prefError, setPrefError] = useState(false);
  const [savingPref, setSavingPref] = useState(false);
  const [markingRead, setMarkingRead] = useState(false);
  const [reload, setReload] = useState(0);
  const [prefReload, setPrefReload] = useState(0);
  const box = useRef<HTMLDivElement>(null);
  const bell = useRef<HTMLButtonElement>(null);
  const heading = useRef<HTMLHeadingElement>(null);
  const loadRetry = useRef<HTMLButtonElement>(null);
  const prefRetry = useRef<HTMLButtonElement>(null);
  const readButton = useRef<HTMLButtonElement>(null);
  const panelId = useId();

  useEffect(() => {
    let stop = false;
    let pending = false;
    const load = async () => {
      if (pending) return;
      pending = true;
      try {
        const notices = await api.notices();
        if (!stop) {
          if (document.activeElement === loadRetry.current) heading.current?.focus();
          setItems(notices);
          setLoadError(false);
        }
      } catch {
        if (!stop) setLoadError(true);
      } finally {
        pending = false;
      }
    };
    void load();
    const timer = window.setInterval(() => void load(), 20_000);
    return () => {
      stop = true;
      window.clearInterval(timer);
    };
  }, [reload]);

  useEffect(() => {
    let stop = false;
    void api.getPref("notices.os").then((value) => {
      if (!stop) {
        if (document.activeElement === prefRetry.current) heading.current?.focus();
        setOsOn(value !== "off");
        setPrefError(false);
      }
    }).catch(() => {
      if (!stop) setPrefError(true);
    });
    return () => { stop = true; };
  }, [prefReload]);

  useEffect(() => {
    if (!open) return;
    const close = (event: MouseEvent) => {
      if (box.current && !box.current.contains(event.target as Node)) setOpen(false);
    };
    const escape = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        setOpen(false);
        bell.current?.focus();
      }
    };
    window.addEventListener("mousedown", close);
    window.addEventListener("keydown", escape);
    return () => {
      window.removeEventListener("mousedown", close);
      window.removeEventListener("keydown", escape);
    };
  }, [open]);

  const unread = items?.filter((notice) => notice.unread).length ?? 0;

  async function markRead() {
    if (!items || markingRead || loadError || unread === 0) return;
    const displayed = new Set(items.map((notice) => notice.id));
    setMarkingRead(true);
    setReadError(false);
    try {
      await api.noticesRead([...displayed]);
      if (document.activeElement === readButton.current) heading.current?.focus();
      setItems((current) => current?.map((notice) => displayed.has(notice.id) ? { ...notice, unread: false } : notice) ?? null);
      setReload((value) => value + 1);
    } catch {
      setReadError(true);
    } finally {
      setMarkingRead(false);
    }
  }

  async function switchOs() {
    if (osOn === null || savingPref) return;
    const next = !osOn;
    setSavingPref(true);
    setPrefError(false);
    try {
      await api.setPref("notices.os", next ? "on" : "off");
      setOsOn(next);
    } catch {
      setPrefError(true);
    } finally {
      setSavingPref(false);
    }
  }

  return (
    <div className="relative" ref={box}>
      <button
        ref={bell}
        type="button"
        onClick={() => setOpen((value) => !value)}
        aria-label={loadError ? "Notifications, unable to refresh" : unread ? `Notifications, ${unread} unread` : "Notifications"}
        aria-controls={panelId}
        aria-expanded={open}
        title={loadError ? "Could not refresh notifications" : unread ? `${unread} unread notification${unread === 1 ? "" : "s"}` : "Notifications"}
        className={`relative inline-flex h-8 w-8 items-center justify-center rounded-lg ring-1 transition-colors ${
          open ? "bg-white/[0.06] text-slate-100 ring-line" : "text-slate-300 ring-line hover:bg-white/[0.04]"
        }`}
      >
        {loadError ? <CircleAlert className="size-4 text-amber-300" /> : <Bell className="size-4" />}
        {unread > 0 && (
          <span aria-hidden="true" className="absolute -right-1 -top-1 rounded-full bg-emerald-500 px-1.5 text-2xs font-bold leading-4 text-emerald-950">
            {unread > 99 ? "99+" : unread}
          </span>
        )}
      </button>
      {open && (
        <section id={panelId} aria-label="Notifications" className="absolute right-0 top-10 z-50 w-96 max-w-[calc(100vw-2rem)] overflow-hidden rounded-xl border border-line bg-panel shadow-2xl">
          <div className="flex items-center justify-between gap-2 border-b border-line px-4 py-2.5">
            <h2 ref={heading} tabIndex={-1} className="text-sm font-semibold text-slate-100">Notifications</h2>
            {unread > 0 && (
              <button ref={readButton} type="button" onClick={() => void markRead()} aria-disabled={markingRead || loadError} className="rounded-md px-2 py-1 text-label text-emerald-300 hover:bg-white/[0.05] aria-disabled:opacity-50">
                {markingRead ? "Marking read" : "Mark all read"}
              </button>
            )}
          </div>
          {loadError && (
            <div role="alert" className="border-b border-line px-4 py-3 text-label text-amber-300">
              <p>Could not load notifications.{items !== null && " Showing the last loaded list."}</p>
              <button ref={loadRetry} type="button" onClick={() => setReload((value) => value + 1)} className="mt-1 rounded px-1 underline">Retry</button>
            </div>
          )}
          {readError && <p role="alert" className="border-b border-line px-4 py-3 text-label text-amber-300">Could not mark notifications read. Use Mark all read to retry.</p>}
          <div className="max-h-[60vh] overflow-y-auto">
            {items === null ? (
              !loadError && <p role="status" className="px-4 py-8 text-center text-small text-muted">Loading notifications</p>
            ) : items.length === 0 ? (
              !loadError && <p className="px-4 py-8 text-center text-small text-muted">Nothing needs you. Questions, finished goals and blocked missions appear here.</p>
            ) : (
              items.map((notice) => (
                <button
                  key={notice.id}
                  type="button"
                  onClick={() => {
                    setOpen(false);
                    onGo(notice.view, notice);
                  }}
                  className={`flex w-full gap-3 border-b border-line px-4 py-3 text-left transition-colors last:border-0 hover:bg-white/[0.04] ${notice.unread ? "bg-emerald-500/[0.04]" : ""}`}
                >
                  <span className="mt-0.5 shrink-0">{ICON[notice.kind] ?? <Bell className="size-4" />}</span>
                  <span className="min-w-0 flex-1">
                    <span className="flex items-baseline justify-between gap-2">
                      <span className="truncate text-sm font-medium text-slate-100">{notice.title}</span>
                      <span className="shrink-0 text-2xs text-muted">{ago(notice.at)}</span>
                    </span>
                    <span className="mt-0.5 block truncate font-mono text-2xs text-slate-500">{notice.entity_id}{notice.repo ? ` · ${notice.repo}` : ""}</span>
                    <span className="mt-0.5 line-clamp-2 block text-label text-muted">{notice.detail}</span>
                    {notice.open && <span className="mt-1 inline-block rounded-full bg-amber-500/10 px-1.5 text-2xs text-amber-300 ring-1 ring-amber-500/30">needs you</span>}
                  </span>
                </button>
              ))
            )}
          </div>
          <div className="border-t border-line px-4 py-2">
            {prefError && <p role="alert" className="mb-1 text-label text-amber-300">{osOn === null ? "Could not load Windows alert preference." : "Could not save Windows alert preference."}</p>}
            {osOn === null ? (
              prefError ? <button ref={prefRetry} type="button" onClick={() => setPrefReload((value) => value + 1)} className="rounded px-1 text-label text-muted underline">Retry Windows alert preference</button>
                : <span className="text-label text-muted">Loading Windows alert preference</span>
            ) : (
              <button
                type="button"
                onClick={() => void switchOs()}
                disabled={savingPref}
                aria-pressed={osOn}
                title={osOn ? "Turn Windows notifications off" : "Turn Windows notifications on"}
                className="inline-flex items-center gap-1.5 rounded-md px-2 py-1 text-label text-muted hover:bg-white/[0.05] hover:text-slate-200 disabled:opacity-50"
              >
                {osOn ? <Bell className="size-3.5" /> : <BellOff className="size-3.5" />}
                {savingPref ? "Saving Windows alerts" : osOn ? "Windows alerts on" : "Windows alerts off"}
              </button>
            )}
          </div>
        </section>
      )}
    </div>
  );
}
