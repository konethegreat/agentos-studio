import { FolderGit2, LayoutDashboard, MessagesSquare, PlugZap, ScrollText, SlidersHorizontal } from "lucide-react";
import { useCallback, useEffect, useRef, useState, type ReactNode } from "react";
import {
  api,
  errorText,
  events,
  type Board,
  type HermesStatus,
  type RepoInfo,
  type SettingsView,
  type SetupEvent,
  type StackStatus,
  type WorkerEvent,
  type WorkerInfo,
} from "./lib/api";
import { basename } from "./lib/format";
import { CliDock, CliProvider } from "./components/CliDrawer";
import { CommandBar } from "./components/CommandBar";
import { StatusBar } from "./components/StatusBar";
import { ToastProvider, useToast } from "./components/Toasts";
import { WorkerDrawer } from "./components/WorkerDrawer";
import { ChatStudio } from "./views/ChatStudio";
import { ConstitutionStudio } from "./views/ConstitutionStudio";
import { ControlPlane } from "./views/ControlPlane";
import { FleetManager } from "./views/FleetManager";
import { MissionControl } from "./views/MissionControl";
import { SetupWizard } from "./views/SetupWizard";

type View = "chat" | "mission" | "control" | "fleet" | "constitution" | "setup";

const NAV: { id: View; label: string; icon: ReactNode }[] = [
  { id: "chat", label: "Hermes Chat Studio", icon: <MessagesSquare className="size-4" /> },
  { id: "mission", label: "Mission Control", icon: <LayoutDashboard className="size-4" /> },
  { id: "control", label: "MCP and Providers", icon: <SlidersHorizontal className="size-4" /> },
  { id: "fleet", label: "Fleet Repositories", icon: <FolderGit2 className="size-4" /> },
  { id: "constitution", label: "Constitution and Memory", icon: <ScrollText className="size-4" /> },
  { id: "setup", label: "Setup and Integrations", icon: <PlugZap className="size-4" /> },
];

export default function App() {
  return (
    <ToastProvider>
      <Studio />
    </ToastProvider>
  );
}

function Studio() {
  const toast = useToast();
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

  useEffect(() => {
    api.getSettings().then(setSettings).catch((e) => toast("error", errorText(e)));
    void loadHermes();
    void loadBoard();
    void loadStatus();
    void scan();
  }, [loadBoard, loadHermes, loadStatus, scan, toast]);

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

  const runningWorker = drawer ? workers.find((w) => w.task_id === drawer) : undefined;
  const home = settings?.home ?? "";

  return (
    <CliProvider home={home} repos={repos}>
      <div className="flex h-full">
        <nav className="flex w-56 shrink-0 flex-col border-r border-line bg-[#0a0e15]">
          <div className="flex items-center gap-2.5 px-5 py-4">
            <img src="/icon.png" alt="" className="size-8 rounded-lg" />
            <div>
              <div className="text-sm font-semibold tracking-tight text-slate-100">AgentOS Studio</div>
              <div className="text-[11px] text-slate-500">v{settings?.version ?? "0.2.0"}</div>
            </div>
          </div>
          <div className="flex-1 space-y-0.5 px-3">
            {NAV.map((n) => (
              <button
                key={n.id}
                onClick={() => go(n.id)}
                className={`flex w-full items-center gap-2.5 rounded-lg px-3 py-2 text-[13px] transition-colors ${
                  view === n.id ? "bg-white/[0.07] font-medium text-slate-100" : "text-slate-400 hover:bg-white/[0.03] hover:text-slate-200"
                }`}
              >
                <span className={view === n.id ? "text-emerald-300" : ""}>{n.icon}</span>
                {n.label}
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
          <div className="relative min-h-0 flex-1">
            {visited.has("chat") && (
              <div className={view === "chat" ? "h-full" : "hidden"}>
                <ChatStudio repos={repos} hermes={hermes} home={home} onStatusChanged={() => void loadHermes()} onOpenControl={openControl} />
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
            {view === "setup" && (
              <SetupWizard status={status} settings={settings} setupLog={setupLog} activeJob={activeJob} onRefresh={loadStatus} onSettings={setSettings} />
            )}
            {view === "fleet" && <FleetManager repos={repos} scanning={scanning} onRescan={scan} />}
            {view === "constitution" && <ConstitutionStudio memoryTick={memoryTick} />}
          </div>
          <CliDock />
        </main>
        {drawer && <WorkerDrawer key={drawer} taskId={drawer} worker={runningWorker} liveLines={lines[drawer] ?? []} onClose={() => setDrawer(null)} />}
      </div>
    </CliProvider>
  );
}
