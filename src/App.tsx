import {
  Activity,
  ArrowUpCircle,
  Bot,
  FolderGit2,
  GitPullRequest,
  LayoutDashboard,
  MessageSquarePlus,
  MessagesSquare,
  PlugZap,
  RefreshCw,
  ScrollText,
  Search,
  ShieldCheck,
  SlidersHorizontal,
  SquareTerminal,
  TerminalSquare,
} from "lucide-react";
import { Suspense, lazy, useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import {
  api,
  errorText,
  events,
  type Board,
  type BootReport,
  type BootStep,
  type HermesStatus,
  type RepoInfo,
  type SettingsView,
  type SetupEvent,
  type StackStatus,
  type WorkerEvent,
  type WorkerInfo,
} from "./lib/api";
import { basename } from "./lib/format";
import { ApprovalProvider } from "./components/ApprovalProvider";
import { BootSplash } from "./components/BootSplash";
import { CliDock, CliProvider, useCli } from "./components/CliDrawer";
import { CommandBar } from "./components/CommandBar";
import { CommandPalette, type PaletteCommand } from "./components/CommandPalette";
import { StatusBar } from "./components/StatusBar";
import { ToastProvider, useToast } from "./components/Toasts";
import { UpdateBanner, UpdatesProvider, useUpdates } from "./components/Updates";
import { WorkerDrawer } from "./components/WorkerDrawer";
import { ChatStudio } from "./views/ChatStudio";
import { ConstitutionStudio } from "./views/ConstitutionStudio";
import { ControlPlane } from "./views/ControlPlane";
import { Diagnostics } from "./views/Diagnostics";
import { FleetManager } from "./views/FleetManager";
import { MissionControl } from "./views/MissionControl";
import { SafetyCenter } from "./views/SafetyCenter";
import { SetupWizard } from "./views/SetupWizard";
import { UpdateCenter } from "./views/UpdateCenter";

const GitHubView = lazy(() => import("./views/GitHubView").then((m) => ({ default: m.GitHubView })));

type View = "chat" | "github" | "mission" | "control" | "fleet" | "constitution" | "updates" | "safety" | "setup" | "diagnostics";

const NAV: { id: View; label: string; icon: ReactNode; hint: string; keys?: string }[] = [
  { id: "chat", label: "Hermes Chat Studio", icon: <MessagesSquare className="size-4" />, hint: "Chat with Hermes or Claude Code, organised in workspaces and folders", keys: "Alt+1" },
  { id: "github", label: "GitHub", icon: <GitPullRequest className="size-4" />, hint: "Pull requests, issues, Actions, branches and commits through gh", keys: "Alt+2" },
  { id: "mission", label: "Mission Control", icon: <LayoutDashboard className="size-4" />, hint: "The Agent Bus board: dispatch and track tasks", keys: "Alt+3" },
  { id: "control", label: "MCP and Providers", icon: <SlidersHorizontal className="size-4" />, hint: "Switch providers and manage MCP servers in every agent", keys: "Alt+4" },
  { id: "fleet", label: "Fleet Repositories", icon: <FolderGit2 className="size-4" />, hint: "Every local repository and its rule-file sync state", keys: "Alt+5" },
  { id: "constitution", label: "Constitution and Memory", icon: <ScrollText className="size-4" />, hint: "Edit and broadcast the agent constitution, browse shared memory", keys: "Alt+6" },
  { id: "updates", label: "Updates", icon: <ArrowUpCircle className="size-4" />, hint: "Hermes, Claude Code and Studio versions, changelogs, backups and rollback", keys: "Alt+7" },
  { id: "safety", label: "Safety", icon: <ShieldCheck className="size-4" />, hint: "Permission policies and the audit log", keys: "Alt+8" },
  { id: "setup", label: "Setup and Integrations", icon: <PlugZap className="size-4" />, hint: "Install, sign in and repair everything", keys: "Alt+9" },
  { id: "diagnostics", label: "Diagnostics", icon: <Activity className="size-4" />, hint: "Startup checks, safe repairs, crash log and boot log" },
];

export default function App() {
  return (
    <ToastProvider>
      <ApprovalProvider>
        <UpdatesProvider>
          <Studio />
        </UpdatesProvider>
      </ApprovalProvider>
    </ToastProvider>
  );
}

function Studio() {
  const toast = useToast();
  const updates = useUpdates();
  const [view, setView] = useState<View>("chat");
  const [controlTab, setControlTab] = useState<"providers" | "mcp">("providers");
  const [settings, setSettings] = useState<SettingsView | null>(null);
  const [status, setStatus] = useState<StackStatus | null>(null);
  const [hermes, setHermes] = useState<HermesStatus | null>(null);
  const [board, setBoard] = useState<Board | null>(null);
  const [workers, setWorkers] = useState<WorkerInfo[]>([]);
  const [repos, setRepos] = useState<RepoInfo[]>([]);
  const [scanning, setScanning] = useState(false);
  const [drawer, setDrawer] = useState<string | null>(null);
  const [lines, setLines] = useState<Record<string, WorkerEvent[]>>({});
  const [setupLog, setSetupLog] = useState<SetupEvent[]>([]);
  const [activeJob, setActiveJob] = useState<string | null>(null);
  const [memoryTick, setMemoryTick] = useState(0);
  const [visited, setVisited] = useState<Set<View>>(() => new Set<View>(["chat"]));
  const [incoming, setIncoming] = useState<{ text: string; cwd?: string; nonce: number } | null>(null);
  const [palette, setPalette] = useState(false);
  const [bootSteps, setBootSteps] = useState<BootStep[]>([]);
  const [bootReport, setBootReport] = useState<BootReport | null>(null);
  const [bootDone, setBootDone] = useState(false);
  const [splash, setSplash] = useState(true);
  const liveBoot = useRef(false);
  const boardTimer = useRef<number | undefined>(undefined);

  const go = useCallback((v: View) => {
    setView(v);
    setVisited((s) => (s.has(v) ? s : new Set(s).add(v)));
  }, []);

  const openControl = useCallback(
    (tab: "providers" | "mcp") => {
      setControlTab(tab);
      go("control");
    },
    [go],
  );

  const askHermes = useCallback(
    (text: string, cwd?: string) => {
      setIncoming({ text, cwd, nonce: Date.now() });
      go("chat");
    },
    [go],
  );

  const loadBoard = useCallback(async () => {
    try {
      const [b, w] = await Promise.all([api.board(), api.workers()]);
      setBoard(b);
      setWorkers(w);
    } catch (e) {
      toast("error", errorText(e));
    }
  }, [toast]);

  const loadStatus = useCallback(async () => {
    try {
      setStatus(await api.status());
    } catch (e) {
      toast("error", errorText(e));
    }
  }, [toast]);

  const loadHermes = useCallback(
    async (refresh = false) => {
      try {
        setHermes(await api.hermesStatus(refresh));
      } catch (e) {
        toast("error", errorText(e));
      }
    },
    [toast],
  );

  const scan = useCallback(async () => {
    setScanning(true);
    try {
      setRepos(await api.scanFleet());
    } catch (e) {
      toast("error", errorText(e));
    } finally {
      setScanning(false);
    }
  }, [toast]);

  // Startup checks: the window paints immediately and the report streams in beside it.
  useEffect(() => {
    const un = events.boot((p) => {
      liveBoot.current = true;
      setBootSteps(p.steps);
      if (p.report) setBootReport(p.report);
      if (p.done) setBootDone(true);
    });
    api
      .bootSnapshot()
      .then((r) => {
        if (!r) return;
        setBootSteps((prev) => (prev.length ? prev : r.steps));
        setBootReport((prev) => prev ?? r);
        // A cached report only counts as done if no live run announced itself, so a stale
        // "ready" from last launch cannot hide a failure found on this one.
        window.setTimeout(() => {
          if (!liveBoot.current && r.ready) setBootDone(true);
        }, 2000);
      })
      .catch(() => undefined);
    return () => void un.then((f) => f());
  }, []);

  // Heavy data loads wait for the checks, so the first paint is never competing with a full
  // stack probe and a fleet scan.
  useEffect(() => {
    if (!bootDone) return;
    api.getSettings().then(setSettings).catch((e) => toast("error", errorText(e)));
    void loadHermes();
    void loadBoard();
    void loadStatus();
    void scan();
  }, [bootDone, loadBoard, loadHermes, loadStatus, scan, toast]);

  useEffect(() => {
    const subs = [
      events.hermesStatus(setHermes),
      events.vaultChanged((p) => {
        window.clearTimeout(boardTimer.current);
        boardTimer.current = window.setTimeout(() => void loadBoard(), 60);
        if (p.memory) setMemoryTick((t) => t + 1);
      }),
      events.worker((ev) => {
        setLines((prev) => {
          const list = prev[ev.task_id] ?? [];
          const next = list.length > 2000 ? list.slice(-1500) : list;
          return { ...prev, [ev.task_id]: [...next, ev] };
        });
        if (ev.stream === "exit" || ev.stream === "system") void loadBoard();
        if (ev.stream === "exit") toast("info", `${ev.task_id}: ${ev.line}`);
      }),
      events.setup((ev) => {
        setSetupLog((prev) => [...prev.slice(-800), ev]);
        if (ev.done) {
          setActiveJob(null);
          toast(ev.ok ? "ok" : "error", `Setup: ${ev.line}`);
          void loadStatus();
          void loadHermes(true);
        } else {
          setActiveJob(ev.job);
        }
      }),
    ];
    return () => {
      subs.forEach((p) => void p.then((un) => un()));
    };
  }, [loadBoard, loadHermes, loadStatus, toast]);

  // Keyboard: Ctrl+K palette, Alt+1..9 views.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "k") {
        e.preventDefault();
        setPalette((p) => !p);
      }
      if (e.altKey && !e.ctrlKey && /^[1-9]$/.test(e.key)) {
        const n = NAV[Number(e.key) - 1];
        if (n) {
          e.preventDefault();
          go(n.id);
        }
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [go]);

  const runningWorker = drawer ? workers.find((w) => w.task_id === drawer) : undefined;
  const home = settings?.home ?? "";

  const bootProblems = bootSteps.filter((s) => s.status === "warn" || s.status === "failed").length;

  return (
    <CliProvider home={home} repos={repos}>
      {splash && (
        <BootSplash
          steps={bootSteps}
          report={bootReport}
          done={bootDone}
          onContinue={() => setSplash(false)}
          onDiagnostics={() => {
            setSplash(false);
            go("diagnostics");
          }}
          onFix={(v) => {
            setSplash(false);
            go(v as View);
          }}
        />
      )}
      <div className="flex h-full">
        <nav className="flex w-56 shrink-0 flex-col border-r border-line bg-[#0a0e15]" aria-label="Main">
          <div className="flex items-center gap-2.5 px-5 py-4">
            <img src="/icon.png" alt="" className="size-8 rounded-lg" />
            <div>
              <div className="text-sm font-semibold tracking-tight text-slate-100">AgentOS Studio</div>
              <div className="text-[11px] text-slate-500">v{settings?.version ?? "0.3.0"}</div>
            </div>
          </div>
          <button
            onClick={() => setPalette(true)}
            className="mx-3 mb-2 flex items-center gap-2 whitespace-nowrap rounded-lg bg-white/[0.03] px-3 py-1.5 text-left text-[12px] text-slate-500 ring-1 ring-line hover:text-slate-300"
            title="Search every command and screen"
          >
            <Search className="size-3.5 shrink-0" />
            <span className="truncate">Search commands</span>
            <kbd className="ml-auto shrink-0 rounded bg-white/5 px-1 text-[10px]">Ctrl K</kbd>
          </button>
          <div className="flex-1 space-y-0.5 px-3">
            {NAV.map((n) => (
              <button
                key={n.id}
                onClick={() => go(n.id)}
                title={`${n.hint}${n.keys ? ` (${n.keys})` : ""}`}
                aria-current={view === n.id ? "page" : undefined}
                className={`flex w-full items-center gap-2.5 rounded-lg px-3 py-2 text-[13px] transition-colors ${
                  view === n.id ? "bg-white/[0.07] font-medium text-slate-100" : "text-slate-400 hover:bg-white/[0.03] hover:text-slate-200"
                }`}
              >
                <span className={view === n.id ? "text-emerald-300" : ""}>{n.icon}</span>
                {n.label}
                {n.id === "updates" && updates.alerts.length > 0 && <span className="ml-auto rounded-full bg-emerald-500 px-1.5 text-[10px] font-semibold text-emerald-950">{updates.alerts.length}</span>}
                {n.id === "diagnostics" && bootProblems > 0 && (
                  <span className="ml-auto rounded-full bg-amber-500/20 px-1.5 text-[10px] font-semibold text-amber-300 ring-1 ring-amber-500/30">{bootProblems}</span>
                )}
              </button>
            ))}
          </div>
          <div className="border-t border-line px-5 py-4 text-[11px] leading-relaxed text-slate-500">
            <div className="font-medium text-slate-400">Vault</div>
            <div className="truncate" title={settings?.vault}>
              {settings ? basename(settings.vault) : "loading"}
            </div>
            <div className="mt-2 flex gap-3">
              <span>{board ? board.pending.length + board.in_progress.length : 0} open</span>
              <span>{repos.length} repos</span>
              <span>{hermes?.sessions ?? 0} chats</span>
            </div>
          </div>
        </nav>
        <main className="flex min-w-0 flex-1 flex-col">
          <StatusBar hermes={hermes} stack={status} onOpenSetup={() => go("setup")} onOpenControl={openControl} onRefresh={() => void loadHermes(true)} />
          <UpdateBanner onOpen={() => go("updates")} />
          <div className="relative min-h-0 flex-1">
            {visited.has("chat") && (
              <div className={view === "chat" ? "h-full" : "hidden"}>
                <ChatStudio repos={repos} hermes={hermes} home={home} onStatusChanged={() => void loadHermes()} onOpenControl={openControl} incoming={incoming} />
              </div>
            )}
            {visited.has("github") && (
              <div className={view === "github" ? "h-full" : "hidden"}>
                <Suspense fallback={null}>
                  <GitHubView repos={repos} onAskHermes={askHermes} />
                </Suspense>
              </div>
            )}
            {view === "mission" && (
              <div className="flex h-full flex-col">
                <CommandBar
                  repos={repos}
                  onDispatched={(id, hasWorker) => {
                    void loadBoard();
                    if (hasWorker) setDrawer(id);
                  }}
                />
                <div className="min-h-0 flex-1">
                  <MissionControl board={board} workers={workers} onOpen={setDrawer} />
                </div>
              </div>
            )}
            {view === "control" && <ControlPlane tab={controlTab} onTab={setControlTab} onStatusChanged={() => void loadHermes()} />}
            {view === "setup" && <SetupWizard status={status} settings={settings} setupLog={setupLog} activeJob={activeJob} onRefresh={loadStatus} onSettings={setSettings} />}
            {view === "fleet" && <FleetManager repos={repos} scanning={scanning} onRescan={scan} />}
            {view === "constitution" && <ConstitutionStudio memoryTick={memoryTick} />}
            {view === "updates" && <UpdateCenter />}
            {view === "safety" && <SafetyCenter />}
            {view === "diagnostics" && <Diagnostics onFix={(v) => go(v as View)} />}
          </div>
          <CliDock />
        </main>
        {drawer && <WorkerDrawer key={drawer} taskId={drawer} worker={runningWorker} liveLines={lines[drawer] ?? []} onClose={() => setDrawer(null)} />}
        <PaletteHost open={palette} onClose={() => setPalette(false)} go={go} askHermes={askHermes} onRescan={() => void scan()} />
      </div>
    </CliProvider>
  );
}

/** Lives inside CliProvider so commands can open terminals. */
function PaletteHost({ open, onClose, go, askHermes, onRescan }: { open: boolean; onClose: () => void; go: (v: View) => void; askHermes: (t: string, cwd?: string) => void; onRescan: () => void }) {
  const cli = useCli();
  const updates = useUpdates();
  const commands = useMemo<PaletteCommand[]>(
    () => [
      { id: "new-chat", group: "Chat", label: "New chat", hint: "Start a new Hermes conversation", icon: <MessageSquarePlus className="size-4" />, run: () => askHermes("") },
      ...NAV.map((n) => ({ id: `go-${n.id}`, group: "Go to", label: n.label, hint: n.hint, keys: n.keys, icon: n.icon, run: () => go(n.id) })),
      { id: "term-toggle", group: "Terminal", label: "Show or hide the terminal", keys: "Ctrl+`", icon: <SquareTerminal className="size-4" />, run: () => cli.setOpen(!cli.open) },
      { id: "term-shell", group: "Terminal", label: "New shell terminal", hint: "PowerShell or your login shell", icon: <TerminalSquare className="size-4" />, run: () => void cli.openPty({ profile: "shell" }) },
      { id: "term-hermes", group: "Terminal", label: "New interactive Hermes terminal", hint: "Live reasoning, slash commands, typed approvals", icon: <Bot className="size-4" />, run: () => void cli.openPty({ profile: "hermes" }) },
      { id: "term-hermes-tui", group: "Terminal", label: "New Hermes TUI terminal", icon: <Bot className="size-4" />, run: () => void cli.openPty({ profile: "hermes-tui" }) },
      { id: "term-claude", group: "Terminal", label: "New Claude Code terminal", icon: <Bot className="size-4" />, run: () => void cli.openPty({ profile: "claude" }) },
      { id: "hermes-doctor", group: "Hermes", label: "Run hermes doctor", icon: <SquareTerminal className="size-4" />, run: () => void cli.run("hermes doctor") },
      { id: "hermes-status", group: "Hermes", label: "Run hermes status", icon: <SquareTerminal className="size-4" />, run: () => void cli.run("hermes status") },
      { id: "hermes-insights", group: "Hermes", label: "Token and cost usage (hermes insights, 7 days)", icon: <SquareTerminal className="size-4" />, run: () => void cli.run("hermes insights --days 7") },
      { id: "updates-check", group: "Updates", label: "Check for updates now", hint: `${updates.alerts.length} waiting`, icon: <RefreshCw className="size-4" />, run: () => void updates.check().then(() => go("updates")) },
      { id: "fleet-rescan", group: "Fleet", label: "Rescan repositories", icon: <FolderGit2 className="size-4" />, run: onRescan },
    ],
    [askHermes, cli, go, onRescan, updates],
  );
  return <CommandPalette commands={commands} open={open} onClose={onClose} />;
}
