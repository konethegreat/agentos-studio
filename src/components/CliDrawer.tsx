import { ChevronDown, CornerDownLeft, Eraser, Square, SquareTerminal } from "lucide-react";
import { createContext, useCallback, useContext, useEffect, useRef, useState, type KeyboardEvent, type MouseEvent as ReactMouseEvent, type ReactNode } from "react";
import { api, errorText, events, type CliLine, type RepoInfo } from "../lib/api";
import { useToast } from "./Toasts";

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
    return () => void un.then((f) => f());
  }, []);

  const run = useCallback(
    async (command: string, dir?: string) => {
      setOpen(true);
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
    <CliCtx.Provider value={{ run, open, setOpen, lines, running, cwd, setCwd, clear: () => setLines([]), stop, home, repos }}>{children}</CliCtx.Provider>
  );
}

/** The terminal panel, docked at the bottom of the main column so it never covers the composer. */
export function CliDock() {
  const c = useCli();
  if (!c.open) return null;
  return (
    <CliPanel
      lines={c.lines}
      running={c.running}
      cwd={c.cwd}
      home={c.home}
      repos={c.repos}
      onCwd={c.setCwd}
      onRun={(cmd) => void c.run(cmd)}
      onClear={c.clear}
      onStop={c.stop}
      onClose={() => c.setOpen(false)}
    />
  );
}

function CliPanel({
  lines,
  running,
  cwd,
  home,
  repos,
  onCwd,
  onRun,
  onClear,
  onStop,
  onClose,
}: {
  lines: Line[];
  running: string[];
  cwd: string;
  home: string;
  repos: RepoInfo[];
  onCwd: (v: string) => void;
  onRun: (c: string) => void;
  onClear: () => void;
  onStop: (id: string) => void;
  onClose: () => void;
}) {
  const [input, setInput] = useState("");
  const [history, setHistory] = useState<string[]>(() => JSON.parse(localStorage.getItem("cli-history") ?? "[]") as string[]);
  const [hIdx, setHIdx] = useState(-1);
  const [height, setHeight] = useState(() => Number(localStorage.getItem("cli-height") ?? 300));
  const scroller = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const el = scroller.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [lines]);

  function submit(cmd = input) {
    const c = cmd.trim();
    if (!c) return;
    onRun(c);
    const h = [c, ...history.filter((x) => x !== c)].slice(0, 80);
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
    const move = (ev: MouseEvent) => setHeight(Math.max(160, Math.min(window.innerHeight - 160, startH + startY - ev.clientY)));
    const up = (ev: MouseEvent) => {
      localStorage.setItem("cli-height", String(Math.max(160, Math.min(window.innerHeight - 160, startH + startY - ev.clientY))));
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

  return (
    <div className="flex shrink-0 flex-col border-t border-line bg-[#080b11]" style={{ height }}>
      <div onMouseDown={startDrag} className="h-1.5 shrink-0 cursor-ns-resize hover:bg-emerald-500/30" />
      <div className="flex shrink-0 items-center gap-2 border-b border-line px-4 pb-2">
        <SquareTerminal className="size-4 text-emerald-400" />
        <span className="text-xs font-semibold text-slate-200">Terminal</span>
        <select
          value={cwd}
          onChange={(e) => onCwd(e.target.value)}
          className="h-7 max-w-60 rounded-md border border-line bg-card px-2 text-[11px] text-slate-300 outline-none"
          aria-label="Working directory"
        >
          <option value="">{home || "Home"}</option>
          {repos.map((r) => (
            <option key={r.path} value={r.path}>
              {r.name}
            </option>
          ))}
        </select>
        <div className="flex min-w-0 flex-1 flex-wrap gap-1">
          {QUICK.map((q) => (
            <button key={q} onClick={() => submit(q)} className="h-6 rounded-md bg-white/[0.04] px-2 font-mono text-[10.5px] text-slate-400 ring-1 ring-white/5 hover:bg-white/[0.08] hover:text-slate-100">
              {q}
            </button>
          ))}
        </div>
        {running.length > 0 && (
          <button onClick={() => running.forEach(onStop)} className="inline-flex h-6 items-center gap-1 rounded-md bg-red-500/10 px-2 text-[11px] text-red-300 ring-1 ring-red-500/30 hover:bg-red-500/20">
            <Square className="size-3" /> Stop ({running.length})
          </button>
        )}
        <button onClick={onClear} className="rounded-md p-1 text-slate-500 hover:bg-white/5 hover:text-slate-200" title="Clear" aria-label="Clear terminal">
          <Eraser className="size-4" />
        </button>
        <button onClick={onClose} className="rounded-md p-1 text-slate-500 hover:bg-white/5 hover:text-slate-200" title="Hide terminal (Ctrl+`)" aria-label="Hide terminal">
          <ChevronDown className="size-4" />
        </button>
      </div>
      <div ref={scroller} className="min-h-0 flex-1 overflow-y-auto px-4 py-2 font-mono text-[12px] leading-[1.55]">
        {lines.length === 0 && <div className="text-slate-600">Run any hermes, claude, git, gh or PowerShell command. Up and Down walk through history.</div>}
        {lines.map((l) => (
          <div key={l.id} className={`whitespace-pre-wrap break-words ${tone[l.stream]} ${l.stream === "start" ? "mt-2" : ""}`}>
            {l.text || "\u00a0"}
          </div>
        ))}
      </div>
      <div className="flex shrink-0 items-center gap-2 border-t border-line px-4 py-2">
        <span className="font-mono text-xs text-emerald-400">&gt;</span>
        <input
          autoFocus
          value={input}
          onChange={(e) => setInput(e.target.value)}
          onKeyDown={onKey}
          placeholder="hermes mcp test agentos-hub"
          className="min-w-0 flex-1 bg-transparent font-mono text-[12.5px] text-slate-100 outline-none placeholder:text-slate-600"
        />
        <button onClick={() => submit()} className="inline-flex items-center gap-1 rounded-md px-2 py-1 text-[11px] text-slate-400 hover:bg-white/5 hover:text-slate-100">
          <CornerDownLeft className="size-3.5" /> Run
        </button>
      </div>
    </div>
  );
}
