import {
  Brain,
  Bot,
  FolderGit2,
  GitPullRequest,
  LayoutDashboard,
  MessageSquarePlus,
  MessagesSquare,
  RefreshCw,
  SlidersHorizontal,
  SquareTerminal,
  Sunrise,
  TerminalSquare,
} from "lucide-react";
import { Suspense, lazy, useCallback, useEffect, useMemo, useRef, useState, } from "react";
import {
  api,
  errorText,
  events,
  type Board,
  type BootReport,
  type BootStep,
  type HermesStatus,
  type ProjectSession,
  type RepoInfo,
  type SettingsView,
  type SetupEvent,
  type StackStatus,
  type WorkerEvent,
  type WorkerInfo,
} from "./lib/api";
import { ApprovalProvider } from "./components/ApprovalProvider";
import { BootSplash } from "./components/BootSplash";
import { CliDock, CliProvider, useCli } from "./components/CliDrawer";
import { CommandBar } from "./components/CommandBar";
import { CommandPalette, type PaletteCommand } from "./components/CommandPalette";
import { StatusBar } from "./components/StatusBar";
import { Memory } from "./views/Memory";
import { Onboarding } from "./components/Onboarding";
import { Help } from "./views/Help";
import { GuideProvider } from "./components/Guides";
import { Missions } from "./views/Missions";
import { Goals } from "./views/Goals";
import { NavRail, type RailGroup, type RailView } from "./components/NavRail";
import { ToastProvider, useToast } from "./components/Toasts";
import { UpdateBanner, UpdatesProvider, useUpdates } from "./components/Updates";
import { WorkerDrawer } from "./components/WorkerDrawer";
import { ChatStudio, type ChatIntent } from "./views/ChatStudio";
import { ConstitutionStudio } from "./views/ConstitutionStudio";
import { ControlPlane } from "./views/ControlPlane";
import { Diagnostics } from "./views/Diagnostics";
import { FleetManager } from "./views/FleetManager";
import { Agents } from "./views/Agents";
import { Timeline } from "./views/Timeline";
import { MissionControl } from "./views/MissionControl";
import { Projects } from "./views/Projects";
import { SafetyCenter } from "./views/SafetyCenter";
import { SetupWizard } from "./views/SetupWizard";
import { Skills } from "./views/Skills";
import { Capabilities } from "./views/Capabilities";
import { ContextLedger } from "./views/ContextLedger";
import { Prompts } from "./views/Prompts";
import { Autonomy } from "./views/Autonomy";
import { Today } from "./views/Today";
import { UpdateCenter } from "./views/UpdateCenter";

const GitHubView = lazy(() => import("./views/GitHubView").then((m) => ({ default: m.GitHubView })));
const ComputerUse = lazy(() => import("./views/ComputerUse").then((m) => ({ default: m.ComputerUse })));

const VIEWS = ["today", "chat", "goals", "autonomy", "projects", "prompts", "skills", "capabilities", "memory", "context", "help", "github", "mission", "control", "fleet", "agents", "timeline", "computer", "constitution", "updates", "safety", "setup", "diagnostics"] as const;
type View = (typeof VIEWS)[number];
const isView = (v: string): v is View => (VIEWS as readonly string[]).includes(v);

const GROUPS: RailGroup[] = [
  {
    id: "today",
    label: "Today",
    icon: <Sunrise className="size-4" />,
    defaultView: "today",
    tabs: [{ id: "today", label: "Today", hint: "What waits on you, goals in progress, spend, running agents and the last digest" }],
  },
  {
    id: "chat",
    label: "Chat",
    icon: <MessagesSquare className="size-4" />,
    defaultView: "chat",
    tabs: [{ id: "chat", label: "Chat Studio", hint: "Chat with Hermes or Claude Code, organised in workspaces and folders" }],
  },
  {
    id: "work",
    label: "Work",
    icon: <LayoutDashboard className="size-4" />,
    defaultView: "projects",
    tabs: [
      { id: "goals", label: "Goals", hint: "One sentence in, finished work out: outcomes, done checks, autonomy and the missions each goal created" },
      { id: "autonomy", label: "Autonomy", hint: "Repositories that keep themselves busy: each keeps docs/agentos/PHASES.md and TASKS.md, and Studio keeps one goal in flight for it" },
      { id: "projects", label: "Projects", hint: "Hermes projects, their folders, and the chats that ran inside them" },
      { id: "prompts", label: "Prompts", hint: "Saved prompts that know their project: draft from the repo, check, sharpen, and hand work to another agent or app" },
      { id: "mission", label: "Mission Control", hint: "The Agent Bus board: dispatch and track tasks" },
      { id: "fleet", label: "Fleet", hint: "Every local repository and its rule-file sync state" },
      { id: "timeline", label: "Run timeline", hint: "Who did what on a mission across agents: handoffs, asks, tool calls, approvals, cost and duration, with replay" },
      { id: "agents", label: "Agents", hint: "Fleet agents: how each starts, strengths, cost model, quota headroom and health" },
      { id: "computer", label: "Computer Use", hint: "What Hermes did on the desktop, screenshots, and the switch for it" },
    ],
  },
  {
    id: "brain",
    label: "Brain",
    icon: <Brain className="size-4" />,
    defaultView: "memory",
    tabs: [
      { id: "memory", label: "Memory", hint: "Notes in the vault, how connected they are, and what chats read or wrote" },
      { id: "context", label: "Context Ledger", hint: "What each session kept and dropped on compaction, anchored vault summaries, and the four-probe eval" },
      { id: "skills", label: "Skills", hint: "What Hermes can load, plus hub search and install" },
      { id: "capabilities", label: "Capabilities", hint: "Every skill, plugin, agent and MCP server across Hermes, Qoder and Claude Code, with provenance, duplicates and compatibility (read only)" },
      { id: "constitution", label: "Constitution", hint: "Edit and broadcast the agent constitution, browse shared memory" },
    ],
  },
  {
    id: "code",
    label: "Code",
    icon: <GitPullRequest className="size-4" />,
    defaultView: "github",
    tabs: [{ id: "github", label: "GitHub", hint: "Pull requests, issues, Actions, branches and commits through gh" }],
  },
  {
    id: "system",
    label: "System",
    icon: <SlidersHorizontal className="size-4" />,
    defaultView: "control",
    tabs: [
      { id: "control", label: "MCP and Providers", hint: "Switch providers and manage MCP servers in every agent" },
      { id: "updates", label: "Updates", hint: "Hermes, Claude Code and Studio versions, changelogs, backups and rollback" },
      { id: "safety", label: "Safety", hint: "Permission policies and the audit log" },
      { id: "setup", label: "Setup", hint: "Install, sign in and repair everything" },
      { id: "diagnostics", label: "Diagnostics", hint: "Startup checks, safe repairs, crash log and boot log" },
      { id: "help", label: "Help", hint: "The docs that ship with the build, searchable" },
    ],
  },
];

export default function App() {
  return (
    <ToastProvider>
      <ApprovalProvider>
        <UpdatesProvider>
          <GuideProvider>
            <Studio />
          </GuideProvider>
        </UpdatesProvider>
      </ApprovalProvider>
    </ToastProvider>
  );
}

function Studio() {
  const toast = useToast();
  const updates = useUpdates();
  const [view, setView] = useState<View>("today");
  const [controlTab, setControlTab] = useState<"providers" | "mcp" | "hermes">("providers");
  const [settings, setSettings] = useState<SettingsView | null>(null);
  const [status, setStatus] = useState<StackStatus | null>(null);
  const [hermes, setHermes] = useState<HermesStatus | null>(null);
  const [board, setBoard] = useState<Board | null>(null);
  const [ciFailing, setCiFailing] = useState(0);
  const [workers, setWorkers] = useState<WorkerInfo[]>([]);
  const [repos, setRepos] = useState<RepoInfo[]>([]);
  const [scanning, setScanning] = useState(false);
  const [drawer, setDrawer] = useState<string | null>(null);
  const [lines, setLines] = useState<Record<string, WorkerEvent[]>>({});
  const [setupLog, setSetupLog] = useState<SetupEvent[]>([]);
  const [activeJob, setActiveJob] = useState<string | null>(null);
  const [memoryTick, setMemoryTick] = useState(0);
  const [visited, setVisited] = useState<Set<View>>(() => new Set<View>(["today"]));
  const [incoming, setIncoming] = useState<{ text: string; cwd?: string; nonce: number } | null>(null);
  const [chatIntent, setChatIntent] = useState<ChatIntent | null>(null);
  const [palette, setPalette] = useState(false);
  const [bootSteps, setBootSteps] = useState<BootStep[]>([]);
  const [bootReport, setBootReport] = useState<BootReport | null>(null);
  const [bootDone, setBootDone] = useState(false);
  const [splash, setSplash] = useState(true);
  const liveBoot = useRef(false);
  const boardTimer = useRef<number | undefined>(undefined);

  const go = useCallback((target: View) => {
    // A link to a screen that does not exist must never leave a blank window: show Diagnostics.
    const v: View = isView(target) ? target : "diagnostics";
    setView(v);
    setVisited((s) => (s.has(v) ? s : new Set(s).add(v)));
  }, []);

  // A guide's "Read more" opens one Help document from any view.
  const [helpDoc, setHelpDoc] = useState<{ path: string; nonce: number } | null>(null);
  useEffect(() => {
    const onOpen = (e: Event) => {
      const path = (e as CustomEvent<string>).detail;
      if (!path) return;
      setHelpDoc({ path, nonce: Date.now() });
      go("help");
    };
    window.addEventListener("agentos:open-help", onOpen);
    return () => window.removeEventListener("agentos:open-help", onOpen);
  }, [go]);

  // Empty states elsewhere send the user to the screen that fixes them (usually Setup).
  useEffect(() => {
    const onGo = (e: Event) => {
      const v = (e as CustomEvent<string>).detail;
      if (v) go(v as View);
    };
    window.addEventListener("agentos:go", onGo);
    return () => window.removeEventListener("agentos:go", onGo);
  }, [go]);

  const openControl = useCallback(
    (tab: "providers" | "mcp" | "hermes") => {
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

  const openSessionInChat = useCallback(
    (s: ProjectSession) => {
      setChatIntent({ kind: "open", id: s.id, cwd: s.cwd, title: s.title, nonce: Date.now() });
      go("chat");
    },
    [go],
  );

  const newSessionInProject = useCallback(
    (cwd: string, projectSlug: string | null, parent: string | null) => {
      setChatIntent({ kind: "new", cwd, projectSlug, parent, nonce: Date.now() });
      go("chat");
    },
    [go],
  );

  const useSkillInChat = useCallback(
    (name: string) => {
      window.dispatchEvent(new CustomEvent("agentos:use-skill", { detail: name }));
      toast("ok", `${name} will load on your next message`);
      go("chat");
    },
    [go, toast],
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

  // Keyboard: Ctrl+K palette, Alt+1..6 groups.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "k") {
        e.preventDefault();
        setPalette((p) => !p);
      }
      if (e.altKey && !e.ctrlKey && /^[1-6]$/.test(e.key)) {
        const g = GROUPS[Number(e.key) - 1];
        if (g) {
          e.preventDefault();
          go(g.defaultView as View);
        }
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [go]);

  const runningWorker = drawer ? workers.find((w) => w.task_id === drawer) : undefined;
  // Failing runs in the folder this machine actually works in, refreshed slowly.
  useEffect(() => {
    let stop = false;
    const load = () =>
      api
        .workingDirDefault()
        .then((d) => api.ciBadge(d.path))
        .then((n) => {
          if (!stop) setCiFailing(n);
        })
        .catch(() => undefined);
    void load();
    const t = window.setInterval(() => void load(), 180_000);
    return () => {
      stop = true;
      window.clearInterval(t);
    };
  }, []);

  // Where each rail count comes from, so the badge names its source and the tab that holds it.
  const BADGE_SOURCE: Record<string, Pick<RailGroup, "badgeLabel" | "badgeTab">> = {
    work: { badgeLabel: "Agent Bus tasks pending or in progress", badgeTab: "mission" },
    system: { badgeLabel: "updates available", badgeTab: "updates" },
    code: { badgeLabel: "failing CI runs", badgeTab: "github" },
  };
  const railGroups = useMemo<RailGroup[]>(
    () =>
      GROUPS.map((g) => ({
        ...g,
        ...(BADGE_SOURCE[g.id] ?? {}),
        badge:
          g.id === "work"
            ? (board ? board.pending.length + board.in_progress.length : 0)
            : g.id === "system"
              ? updates.alerts.length
              : g.id === "code"
                ? ciFailing
                : 0,
      })),
    [board, updates.alerts.length, ciFailing],
  );

  const home = settings?.home ?? "";


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
      <Onboarding onGo={(v) => go(v as View)} />
      <div className="flex h-full">
        <NavRail groups={railGroups} view={view as RailView} onGo={(v) => go(v as View)} />
        <main className="flex min-w-0 flex-1 flex-col">
          <StatusBar hermes={hermes} stack={status} onOpenDiagnostics={() => go("diagnostics")} onRefresh={() => void loadHermes(true)} onGo={(v) => go(v as View)} />
          <UpdateBanner onOpen={() => go("updates")} />
          <div className="relative min-h-0 flex-1">
            {view === "today" && <Today />}
            {visited.has("chat") && (
              <div className={view === "chat" ? "h-full" : "hidden"}>
                <ChatStudio repos={repos} hermes={hermes} home={home} intent={chatIntent} onStatusChanged={() => void loadHermes()} onOpenControl={openControl} incoming={incoming} active={view === "chat"} />
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
                <Missions repos={repos.map((r) => r.name)} />
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
            {view === "agents" && <Agents />}
            {view === "goals" && <Goals />}
            {view === "timeline" && <Timeline />}
            {view === "constitution" && <ConstitutionStudio memoryTick={memoryTick} />}
            {view === "updates" && <UpdateCenter />}
            {view === "safety" && <SafetyCenter />}
            {view === "diagnostics" && <Diagnostics onFix={(v) => go(v as View)} />}
            {view === "computer" && <ComputerUse />}
            {view === "projects" && <Projects onOpenSession={openSessionInChat} onNewSession={newSessionInProject} />}
            {view === "skills" && <Skills onUseSkill={useSkillInChat} />}
            {view === "capabilities" && <Capabilities />}
            {view === "memory" && <Memory />}
            {view === "context" && <ContextLedger />}
            {view === "prompts" && <Prompts />}
            {view === "autonomy" && <Autonomy onOpenGoals={() => go("goals")} />}
            {view === "help" && <Help doc={helpDoc} />}
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
      ...GROUPS.flatMap((g) =>
        g.tabs.map((t) => ({ id: `go-${t.id}`, group: "Go to", label: `${g.label}: ${t.label}`, hint: t.hint, icon: g.icon, run: () => go(t.id as View) })),
      ),
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
