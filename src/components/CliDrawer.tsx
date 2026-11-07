import { Bot, ChevronDown, CornerDownLeft, Eraser, Plus, Square, SquareTerminal, TerminalSquare, X } from "lucide-react";
import { Suspense, createContext, lazy, useCallback, useContext, useEffect, useRef, useState, type KeyboardEvent, type MouseEvent as ReactMouseEvent, type ReactNode } from "react";
import { api, errorText, events, type CliLine, type RepoInfo } from "../lib/api";
import { studio, studioEvents, type PtyInfo, type PtySpawn } from "../lib/studio";
import { useToast } from "./Toasts";

// xterm.js is only loaded once the first real terminal opens.
const PtyTerminal = lazy(() => import("./PtyTerminal").then((m) => ({ default: m.PtyTerminal })));

interface PtyTab extends PtyInfo {
  exited: boolean;
}

interface CliApi {
  run: (command: string, cwd?: string) => Promise<void>;
  open: boolean;
  setOpen: (v: boolean) => void;
  lines: Line[];
  running: string[];
  cwd: string;
  setCwd: (v: string) => void;
  clear: () => void;
  stop: (id: string) => void;
  home: string;
  repos: RepoInfo[];
  ptys: PtyTab[];
  tab: string;
  setTab: (t: string) => void;
  openPty: (s: PtySpawn) => Promise<PtyInfo | null>;
  closePty: (id: string) => void;
  typeInto: (id: string, text: string) => void;
}

const CliCtx = createContext<CliApi>({
  run: async () => undefined,
  open: false,
  setOpen: () => undefined,
  lines: [],
  running: [],
  cwd: "",
  setCwd: () => undefined,
  clear: () => undefined,
  stop: () => undefined,
  home: "",
  repos: [],
  ptys: [],
  tab: "commands",
  setTab: () => undefined,
  openPty: async () => null,
  closePty: () => undefined,
  typeInto: () => undefined,
});

export function useCli() {
  return useContext(CliCtx);
}

const QUICK = ["hermes doctor", "hermes mcp list", "hermes status", "claude auth status", "claude mcp list", "git status", "gh auth status"];

interface Line {
  id: number;
  run: string;
  stream: CliLine["stream"];
  text: string;
}

export function CliProvider({ children, home, repos }: { children: ReactNode; home: string; repos: RepoInfo[] }) {
  const toast = useToast();
  const [open, setOpen] = useState(false);
  const [lines, setLines] = useState<Line[]>([]);
  const [running, setRunning] = useState<string[]>([]);
  const [cwd, setCwd] = useState("");
  const [ptys, setPtys] = useState<PtyTab[]>([]);
  const [tab, setTab] = useState("commands");
  const counter = useRef(0);

  useEffect(() => {
    const un = events.cli((e) => {
      setLines((prev) => {
        const next = prev.length > 4000 ? prev.slice(-3000) : prev;
        const text = e.stream === "exit" ? `[${e.line}${e.duration_ms !== undefined ? ` in ${(e.duration_ms / 1000).toFixed(1)}s` : ""}]` : e.line;
        return [...next, { id: counter.current++, run: e.run_id, stream: e.stream, text }];
      });
      if (e.stream === "exit") setRunning((r) => r.filter((x) => x !== e.run_id));
    });
    const unExit = studioEvents.ptyExit((e) => setPtys((list) => list.map((p) => (p.id === e.id ? { ...p, exited: true } : p))));
    // Terminals survive a UI reload: pick up any that are still running.
    void studio.pty
      .list()
      .then((list) => setPtys((cur) => (cur.length ? cur : list.map((p) => ({ ...p, exited: false })))))
      .catch(() => undefined);
    return () => {
      void un.then((f) => f());
      void unExit.then((f) => f());
    };
  }, []);

  const run = useCallback(
    async (command: string, dir?: string) => {
      setOpen(true);
      setTab("commands");
      try {
        const id = await api.runCli(command, dir ?? (cwd || home));
        setRunning((r) => [...r, id]);
      } catch (e) {
        setLines((prev) => [...prev, { id: counter.current++, run: "", stream: "stderr", text: errorText(e) }]);
        toast("error", errorText(e));
      }
    },
    [cwd, home, toast],
  );

  const openPty = useCallback(
    async (s: PtySpawn) => {
      try {
        const info = await studio.pty.spawn({ cols: 120, rows: 30, ...s, cwd: s.cwd ?? (cwd || home) });
        setPtys((list) => [...list, { ...info, exited: false }]);
        setTab(info.id);
        setOpen(true);
        return info;
      } catch (e) {
        toast("error", errorText(e));
        return null;
      }
    },
    [cwd, home, toast],
  );

  const closePty = useCallback((id: string) => {
    void studio.pty.kill(id).catch(() => undefined);
    setPtys((list) => list.filter((p) => p.id !== id));
    setTab((t) => (t === id ? "commands" : t));
  }, []);

  const typeInto = useCallback((id: string, text: string) => void studio.pty.write(id, text).catch(() => undefined), []);

  useEffect(() => {
    const onKey = (e: globalThis.KeyboardEvent) => {
      if (e.ctrlKey && e.key === "`") {
        e.preventDefault();
        setOpen((o) => !o);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  const stop = useCallback((id: string) => void api.killCli(id).catch((e) => toast("error", errorText(e))), [toast]);

  return (
    <CliCtx.Provider
      value={{ run, open, setOpen, lines, running, cwd, setCwd, clear: () => setLines([]), stop, home, repos, ptys, tab, setTab, openPty, closePty, typeInto }}
    >
      {children}
    </CliCtx.Provider>
  );
}

const NEW_TERMINALS: { profile: PtySpawn["profile"]; label: string; hint: string }[] = [
  { profile: "shell", label: "Shell", hint: "PowerShell (or your login shell) in the selected folder" },
  { profile: "hermes", label: "Hermes", hint: "Interactive Hermes: live reasoning, slash commands and typed approvals" },
  { profile: "hermes-tui", label: "Hermes TUI", hint: "The full-screen Hermes TUI" },
  { profile: "claude", label: "Claude Code", hint: "Interactive Claude Code in the selected folder" },
];

/** The terminal dock. It stays mounted while hidden so running terminals keep their screen. */
export function CliDock() {
  const c = useCli();
  const [input, setInput] = useState("");
  const [history, setHistory] = useState<string[]>(() => JSON.parse(localStorage.getItem("cli-history") ?? "[]") as string[]);
  const [hIdx, setHIdx] = useState(-1);
  const [height, setHeight] = useState(() => Number(localStorage.getItem("cli-height") ?? 320));
  const [menu, setMenu] = useState(false);
  const scroller = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const el = scroller.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [c.lines, c.tab]);

  function submit(cmd = input) {
    const v = cmd.trim();
    if (!v) return;
    void c.run(v);
    const h = [v, ...history.filter((x) => x !== v)].slice(0, 80);
    setHistory(h);
    localStorage.setItem("cli-history", JSON.stringify(h));
    setInput("");
    setHIdx(-1);
  }

  function onKey(e: KeyboardEvent<HTMLInputElement>) {
    if (e.key === "Enter") submit();
    if (e.key === "ArrowUp" && history.length) {
      e.preventDefault();
      const i = Math.min(hIdx + 1, history.length - 1);
      setHIdx(i);
      setInput(history[i]);
    }
    if (e.key === "ArrowDown") {
      e.preventDefault();
      const i = hIdx - 1;
      setHIdx(i);
      setInput(i >= 0 ? history[i] : "");
    }
  }

  function startDrag(e: ReactMouseEvent) {
    const startY = e.clientY;
    const startH = height;
    const clamp = (y: number) => Math.max(180, Math.min(window.innerHeight - 160, startH + startY - y));
    const move = (ev: MouseEvent) => setHeight(clamp(ev.clientY));
    const up = (ev: MouseEvent) => {
      localStorage.setItem("cli-height", String(clamp(ev.clientY)));
      window.removeEventListener("mousemove", move);
      window.removeEventListener("mouseup", up);
    };
    window.addEventListener("mousemove", move);
    window.addEventListener("mouseup", up);
  }

  const tone: Record<CliLine["stream"], string> = {
    start: "text-emerald-300",
    stdout: "text-slate-200",
    stderr: "text-amber-300/90",
    exit: "text-slate-500",
  };
  const onCommands = c.tab === "commands";

  return (
    <div className={`flex shrink-0 flex-col border-t border-line bg-sunken ${c.open ? "" : "hidden"}`} style={{ height }}>
      <div onMouseDown={startDrag} className="h-1.5 shrink-0 cursor-ns-resize hover:bg-emerald-500/30" title="Drag to resize" />
      <div className="flex shrink-0 items-center gap-1 border-b border-line px-3 pb-1.5" role="tablist" aria-label="Terminals">
        <button
          role="tab"
          aria-selected={onCommands}
          onClick={() => c.setTab("commands")}
          title="One-shot commands with captured output"
          className={`inline-flex h-7 items-center gap-1.5 rounded-md px-2.5 text-xs ${onCommands ? "bg-white/[0.07] text-slate-100" : "text-slate-400 hover:text-slate-200"}`}
        >
          <SquareTerminal className="size-3.5 text-ok" /> Commands
          {c.running.length > 0 && <span className="rounded bg-emerald-500/20 px-1 text-2xs text-emerald-300">{c.running.length}</span>}
        </button>
        {c.ptys.map((p) => (
          <div key={p.id} className={`inline-flex h-7 items-center rounded-md ${c.tab === p.id ? "bg-white/[0.07]" : ""}`}>
            <button
              role="tab"
              aria-selected={c.tab === p.id}
              onClick={() => c.setTab(p.id)}
              title={`${p.title} in ${p.cwd}${p.exited ? " (exited)" : ""}`}
              className={`inline-flex items-center gap-1.5 pl-2.5 pr-1 text-xs ${c.tab === p.id ? "text-slate-100" : "text-slate-400 hover:text-slate-200"}`}
            >
              {p.profile.startsWith("hermes") || p.profile.startsWith("claude") ? <Bot className="size-3.5 text-sky-300" /> : <TerminalSquare className="size-3.5 text-slate-400" />}
              <span className={p.exited ? "line-through opacity-60" : ""}>{p.title}</span>
            </button>
            <button
              onClick={() => c.closePty(p.id)}
              className="mr-1 rounded p-0.5 text-slate-500 hover:bg-white/10 hover:text-slate-200"
              title={p.exited ? "Close tab" : "Stop the process and close"}
              aria-label={`Close ${p.title}`}
            >
              <X className="size-3" />
            </button>
          </div>
        ))}
        <div className="relative">
          <button
            onClick={() => setMenu((m) => !m)}
            className="grid size-7 place-items-center rounded-md text-slate-400 hover:bg-white/5 hover:text-slate-100"
            title="New terminal"
            aria-label="New terminal"
            aria-expanded={menu}
          >
            <Plus className="size-4" />
          </button>
          {menu && (
            <div className="absolute left-0 top-8 z-30 w-72 rounded-lg border border-line bg-card p-1 shadow-xl" onMouseLeave={() => setMenu(false)}>
              {NEW_TERMINALS.map((t) => (
                <button
                  key={t.profile}
                  onClick={() => {
                    setMenu(false);
                    void c.openPty({ profile: t.profile });
                  }}
                  className="block w-full rounded-md px-3 py-2 text-left hover:bg-white/5"
                >
                  <div className="text-xs font-medium text-slate-100">{t.label}</div>
                  <div className="text-label text-slate-500">{t.hint}</div>
                </button>
              ))}
            </div>
          )}
        </div>
        <select
          value={c.cwd}
          onChange={(e) => c.setCwd(e.target.value)}
          className="ml-2 h-7 max-w-56 rounded-md border border-line bg-card px-2 text-label text-slate-300 outline-none"
          aria-label="Working directory for new commands and terminals"
          title="Working directory for new commands and terminals"
        >
          <option value="">{c.home || "Home"}</option>
          {c.repos.map((r) => (
            <option key={r.path} value={r.path}>
              {r.name}
            </option>
          ))}
        </select>
        <div className="flex-1" />
        {onCommands && c.running.length > 0 && (
          <button
            onClick={() => c.running.forEach(c.stop)}
            className="inline-flex h-6 items-center gap-1 rounded-md bg-red-500/10 px-2 text-label text-red-300 ring-1 ring-red-500/30 hover:bg-red-500/20"
            title="Stop every running command"
          >
            <Square className="size-3" /> Stop ({c.running.length})
          </button>
        )}
        {onCommands && (
          <button onClick={c.clear} className="rounded-md p-1 text-slate-500 hover:bg-white/5 hover:text-slate-200" title="Clear output" aria-label="Clear output">
            <Eraser className="size-4" />
          </button>
        )}
        <button
          onClick={() => c.setOpen(false)}
          className="rounded-md p-1 text-slate-500 hover:bg-white/5 hover:text-slate-200"
          title="Hide terminal (Ctrl+`). Running terminals keep running."
          aria-label="Hide terminal"
        >
          <ChevronDown className="size-4" />
        </button>
      </div>

      <div className="relative min-h-0 flex-1">
        <Suspense fallback={null}>
          {c.ptys.map((p) => (
            <PtyTerminal key={p.id} id={p.id} active={c.open && c.tab === p.id} exited={p.exited} />
          ))}
        </Suspense>
        {onCommands && (
          <div className="flex h-full flex-col">
            <div className="flex shrink-0 flex-wrap gap-1 px-4 pt-2">
              {QUICK.map((q) => (
                <button
                  key={q}
                  onClick={() => submit(q)}
                  className="h-6 rounded-md bg-white/[0.04] px-2 font-mono text-caption text-slate-400 ring-1 ring-white/5 hover:bg-white/[0.08] hover:text-slate-100"
                  title={`Run ${q}`}
                >
                  {q}
                </button>
              ))}
            </div>
            <div ref={scroller} className="min-h-0 flex-1 overflow-y-auto px-4 py-2 font-mono text-ui leading-[1.55]">
              {c.lines.length === 0 && (
                <div className="text-slate-600">Run any hermes, claude, git, gh or shell command. For interactive programs open a terminal with +. Up and Down walk through history.</div>
              )}
              {c.lines.map((l) => (
                <div key={l.id} className={`whitespace-pre-wrap break-words ${tone[l.stream]} ${l.stream === "start" ? "mt-2" : ""}`}>
                  {l.text || "\u00a0"}
                </div>
              ))}
            </div>
            <div className="flex shrink-0 items-center gap-2 border-t border-line px-4 py-2">
              <span className="font-mono text-xs text-ok">&gt;</span>
              <input
                value={input}
                onChange={(e) => setInput(e.target.value)}
                onKeyDown={onKey}
                placeholder="hermes mcp test agentos-hub"
                aria-label="Command"
                className="min-w-0 flex-1 bg-transparent font-mono text-body-sm text-slate-100 outline-none placeholder:text-slate-600"
              />
              <button onClick={() => submit()} className="inline-flex items-center gap-1 rounded-md px-2 py-1 text-label text-slate-400 hover:bg-white/5 hover:text-slate-100" title="Run (Enter)">
                <CornerDownLeft className="size-3.5" /> Run
              </button>
            </div>
          </div>
        )}
      </div>
    </div>
  );
}
