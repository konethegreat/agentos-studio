import { open } from "@tauri-apps/plugin-dialog";
import { Brain, Cable, CheckCircle2, Circle, CircleAlert, Cloud, Cpu, FolderOpen, Layers, RefreshCw, Sparkles, Wrench } from "lucide-react";
import { useEffect, useRef, useState, type ReactNode } from "react";
import { api, errorText, type HubClient, type SetupEvent, type SetupPlan, type SettingsView, type StackStatus } from "../lib/api";
import { Button, Card, inputClass } from "../components/ui";
import { useToast } from "../components/Toasts";
import { ConnectCard } from "../components/ConnectCard";

const PILLAR_ICON: Record<number, ReactNode> = {
  0: <Layers className="size-5" />,
  1: <Sparkles className="size-5" />,
  2: <Cpu className="size-5" />,
  3: <Brain className="size-5" />,
  4: <Cloud className="size-5" />,
  5: <Cable className="size-5" />,
  6: <Wrench className="size-5" />,
};

/** Plain names for plan steps, so the plan reads as sentences. */
const STEP_NAME: Record<string, string> = {
  git: "Git",
  node: "Node.js",
  gh: "GitHub CLI",
  "claude-cli": "Claude Code",
  path: "PATH fix",
  hermes: "Hermes",
  "hermes-plugin": "Hermes Claude bridge",
  vault: "Obsidian vault",
  "bus-selftest": "hub self-test",
  "mcp-hermes": "hub in Hermes",
  "mcp-claude": "hub in Claude Code",
  "mcp-qoder": "hub in Qoder",
  "mcp-antigravity": "hub in Antigravity",
  "claude-auth": "Claude",
  "hermes-model": "Hermes model",
};
const names = (ids: string[]) => ids.map((i) => STEP_NAME[i] ?? i).join(", ");

/** The one-click plan: what installs with one admin prompt, what runs as you, what you sign in to. */
function SetupEverything({ status, activeJob, onRun }: { status: StackStatus | null; activeJob: string | null; onRun: () => void }) {
  const [plan, setPlan] = useState<SetupPlan | null>(null);
  useEffect(() => {
    let stop = false;
    api
      .setupPlan()
      .then((p) => {
        if (!stop) setPlan(p);
      })
      .catch(() => undefined);
    return () => {
      stop = true;
    };
  }, [status?.checked_at, activeJob]);
  const empty = plan !== null && plan.system.length + plan.user.length + plan.sign_ins.length === 0;
  return (
    <Card className="mb-5 overflow-hidden border-emerald-500/30">
      <div className="flex items-start gap-4 px-5 py-4">
        <Sparkles className="mt-0.5 size-6 shrink-0 text-emerald-300" />
        <div className="min-w-0 flex-1">
          <div className="text-base font-semibold text-slate-100">{empty ? "Everything is set up" : "Set up everything"}</div>
          {plan === null ? (
            <p className="text-small text-muted">Checking this machine...</p>
          ) : empty ? (
            <p className="text-small text-muted">Every required tool is installed, signed in and connected. Optional items below are yours to add.</p>
          ) : (
            <ol className="mt-1 list-decimal space-y-1 pl-5 text-small text-slate-300">
              {plan.system.length > 0 && (
                <li>
                  Install {names(plan.system)}
                  {plan.install_winget ? ", and winget first" : ""}
                  {plan.admin_prompt ? ". Windows asks for admin permission once for all of them." : "."}
                </li>
              )}
              {plan.user.length > 0 && <li>Then set up {names(plan.user)}.</li>}
              {plan.apps.length > 0 && <li>Connect the hub to {plan.apps.join(", ")} (read-only).</li>}
              {plan.sign_ins.length > 0 && (
                <li>Last, sign in to {names(plan.sign_ins)}. Each opens its own window; finish it there.</li>
              )}
            </ol>
          )}
        </div>
        {!empty && (
          <Button variant="primary" size="lg" busy={activeJob !== null} disabled={plan === null} icon={<Sparkles className="size-4" />} onClick={onRun}>
            Set up everything
          </Button>
        )}
      </div>
    </Card>
  );
}

/** Apps that call the hub without running missions: connect or disconnect each one. */
function OtherApps({ activeJob }: { activeJob: string | null }) {
  const toast = useToast();
  const [apps, setApps] = useState<HubClient[] | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const load = () =>
    api
      .clientsList()
      .then(setApps)
      .catch(() => setApps([]));
  useEffect(() => {
    void load();
  }, [activeJob]);
  async function act(c: HubClient) {
    setBusy(c.id);
    try {
      if (c.current) await api.clientDisconnect(c.id);
      else await api.clientConnect(c.id);
      toast("ok", c.current ? `${c.label}: hub removed` : `${c.label}: hub connected. Restart ${c.label} to load it.`);
      await load();
    } catch (e) {
      toast("error", errorText(e));
    } finally {
      setBusy(null);
    }
  }
  if (!apps) return null;
  return (
    <Card className="mb-5 overflow-hidden">
      <div className="border-b border-line px-4 py-3">
        <div className="text-sm font-semibold text-slate-100">Other apps</div>
        <p className="text-label text-muted">
          Claude Desktop, Cursor, VS Code, Windsurf and Codex can read the bus and the shared memory through the hub. They get no agent identity, so they never write as an agent.
        </p>
      </div>
      <ul className="divide-y divide-line">
        {apps.map((c) => (
          <li key={c.id} className="flex items-center gap-3 px-4 py-2.5">
            {c.current ? <CheckCircle2 className="size-4 shrink-0 text-ok" /> : c.present ? <CircleAlert className="size-4 shrink-0 text-warn" /> : <Circle className="size-4 shrink-0 text-slate-600" />}
            <div className="min-w-0 flex-1">
              <div className="text-sm text-slate-200">{c.label}</div>
              <div className="truncate text-label text-muted" title={c.config}>
                {!c.present ? "Not installed" : c.current ? "Connected to this version's hub" : c.registered ? "Connected to another hub build" : "Installed, not connected"}
              </div>
            </div>
            {c.present && (
              <Button size="sm" variant={c.current ? "ghost" : "primary"} busy={busy === c.id} disabled={activeJob !== null} onClick={() => void act(c)}>
                {c.current ? "Disconnect" : c.registered ? "Update" : "Connect"}
              </Button>
            )}
          </li>
        ))}
      </ul>
    </Card>
  );
}

export function SetupWizard({
  status,
  settings,
  setupLog,
  activeJob,
  onRefresh,
  onSettings,
}: {
  status: StackStatus | null;
  settings: SettingsView | null;
  setupLog: SetupEvent[];
  activeJob: string | null;
  onRefresh: () => Promise<void>;
  onSettings: (s: SettingsView) => void;
}) {
  const toast = useToast();
  const [vaultPath, setVaultPath] = useState(settings?.vault ?? "");
  const [refreshing, setRefreshing] = useState(false);
  const [initBusy, setInitBusy] = useState(false);
  const logEnd = useRef<HTMLDivElement>(null);

  useEffect(() => { setVaultPath(settings?.vault ?? ""); }, [settings?.vault]);
  useEffect(() => { logEnd.current?.scrollIntoView({ block: "end" }); }, [setupLog.length]);

  async function refresh() {
    setRefreshing(true);
    await onRefresh();
    setRefreshing(false);
  }

  async function repair(component: string) {
    try {
      await api.repair(component);
      toast(
        "info",
        component === "everything"
          ? "Setup started. Approve the admin prompt once; sign-in windows open at the end."
          : component === "all"
            ? "Auto-setup started. Sign-in windows open where needed."
            : `Working on ${component}`,
      );
    } catch (e) {
      toast("error", errorText(e));
    }
  }

  async function pickVault() {
    const picked = await open({ directory: true, title: "Choose or create your Obsidian vault folder", defaultPath: vaultPath || undefined });
    if (typeof picked === "string") setVaultPath(picked);
  }

  async function initVault() {
    if (!vaultPath.trim()) return;
    setInitBusy(true);
    try {
      const created = await api.scaffoldVault(vaultPath.trim());
      onSettings(await api.getSettings());
      toast("ok", created.length ? `Vault ready: created ${created.length} items` : "Vault already complete and now active");
      await onRefresh();
    } catch (e) {
      toast("error", errorText(e));
    } finally {
      setInitBusy(false);
    }
  }

  const pillars = status?.pillars ?? [];
  const lineColor: Record<SetupEvent["level"], string> = {
    cmd: "text-sky-300",
    out: "text-slate-300",
    err: "text-amber-300/90",
    step: "text-violet-300 font-semibold",
    ok: "text-emerald-300",
    error: "text-red-300",
  };

  return (
    <div className="h-full overflow-y-auto px-6 pb-10 pt-5">
      <SetupEverything status={status} activeJob={activeJob} onRun={() => void repair("everything")} />
      <div className="mb-5">
        <ConnectCard />
      </div>
      <OtherApps activeJob={activeJob} />
      <div className="mb-5 flex items-end justify-between gap-4">
        <div>
          <h1 className="text-lg font-semibold text-slate-100">1-Click Setup and Integrations</h1>
          <p className="text-xs text-muted">
            Six pillars wire Claude, Hermes, Obsidian, Notion and your IDEs into one fleet. Every fix runs the real installer, sign-in or config write.
          </p>
        </div>
        <div className="flex gap-2">
          <Button variant="secondary" busy={refreshing} icon={<RefreshCw className="size-4" />} onClick={refresh}>
            Re-check
          </Button>
          <Button variant="secondary" busy={activeJob !== null} icon={<Wrench className="size-4" />} onClick={() => repair("all")}>
            Repair what is broken
          </Button>
        </div>
      </div>

      <div className="mb-6 flex items-stretch gap-0 overflow-x-auto pb-1">
        {pillars.filter((p) => p.number > 0).map((p, i, arr) => (
          <div key={p.number} className="flex flex-1 items-stretch">
            <div className={`flex h-full min-h-28 min-w-36 flex-1 flex-col items-center justify-center gap-2 rounded-xl border px-3 py-3 text-center ${p.ok ? "border-emerald-500/30 bg-emerald-500/[0.06]" : "border-amber-500/30 bg-amber-500/[0.06]"}`}>
              <div className={p.ok ? "text-emerald-300" : "text-amber-300"}>{PILLAR_ICON[p.number]}</div>
              <div className="text-xs font-semibold text-slate-200">{p.title}</div>
              <div className={`text-label ${p.ok ? "text-ok" : "text-warn"}`}>{p.ok ? "Connected" : "Needs setup"}</div>
            </div>
            {i < arr.length - 1 && <div className={`my-auto h-px w-6 shrink-0 ${p.ok && arr[i + 1].ok ? "bg-emerald-500/50" : "bg-line"}`} />}
          </div>
        ))}
      </div>

      <div className="grid grid-cols-[1fr_420px] gap-5">
        <div className="space-y-4">
          {pillars.map((p) => (
            <Card key={p.number} className="overflow-hidden">
              <div className="flex items-center gap-3 border-b border-line px-4 py-3">
                <div className={p.ok ? "text-emerald-300" : "text-amber-300"}>{PILLAR_ICON[p.number]}</div>
                <div className="text-sm font-semibold text-slate-100">
                  {p.number > 0 ? `${p.number}. ` : ""}
                  {p.title}
                </div>
              </div>
              {p.number === 3 && (
                <div className="flex items-center gap-2 border-b border-line bg-white/[0.015] px-4 py-3">
                  <input className={inputClass} value={vaultPath} onChange={(e) => setVaultPath(e.target.value)} placeholder="Vault folder" />
                  <Button variant="secondary" icon={<FolderOpen className="size-4" />} onClick={pickVault}>
                    Browse
                  </Button>
                  <Button variant="primary" busy={initBusy} onClick={initVault}>
                    Initialize and Use
                  </Button>
                </div>
              )}
              <ul className="divide-y divide-line">
                {status?.components
                  .filter((c) => c.pillar === p.number)
                  .map((c) => (
                    <li key={c.id} className="flex items-center gap-3 px-4 py-2.5">
                      {c.ok ? <CheckCircle2 className="size-4 shrink-0 text-ok" /> : c.optional ? <Circle className="size-4 shrink-0 text-slate-600" /> : <CircleAlert className="size-4 shrink-0 text-warn" />}
                      <div className="min-w-0 flex-1">
                        <div className="text-sm text-slate-200">
                          {c.label}
                          {c.optional && !c.ok && <span className="ml-2 rounded-full bg-white/5 px-1.5 py-0.5 text-2xs text-slate-400 ring-1 ring-white/10">optional</span>}
                        </div>
                        <div className="truncate text-label text-muted" title={c.detail}>
                          {c.detail}
                        </div>
                      </div>
                      <Button size="sm" variant={c.ok || c.optional ? "ghost" : "primary"} disabled={activeJob !== null} onClick={() => repair(c.id)}>
                        {c.action}
                      </Button>
                    </li>
                  ))}
              </ul>
            </Card>
          ))}
        </div>
        <div className="space-y-4">
          <Card className="sticky top-0 flex max-h-[calc(100vh-180px)] flex-col overflow-hidden">
            <div className="flex items-center justify-between border-b border-line px-4 py-3">
              <div className="text-sm font-semibold text-slate-100">Setup console</div>
              {activeJob && <span className="text-label text-emerald-300">running job {activeJob}</span>}
            </div>
            <div className="min-h-64 flex-1 overflow-y-auto bg-inset px-4 py-3 font-mono text-small leading-relaxed">
              {setupLog.length === 0 ? (
                <div className="text-slate-600">Install, sign-in and registration output streams here.</div>
              ) : (
                setupLog.map((l, i) => (
                  <div key={i} className={`whitespace-pre-wrap break-words ${lineColor[l.level]}`}>
                    {l.line}
                  </div>
                ))
              )}
              <div ref={logEnd} />
            </div>
            {status && (
              <div className="border-t border-line px-4 py-2.5 text-label text-muted">
                Bus binary: <span className="font-mono text-slate-400">{status.studio_binary}</span>
              </div>
            )}
          </Card>
        </div>
      </div>
    </div>
  );
}
