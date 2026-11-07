import { open } from "@tauri-apps/plugin-dialog";
import { Brain, Cable, CheckCircle2, Circle, CircleAlert, Cloud, Cpu, FolderOpen, Layers, RefreshCw, Sparkles, Wrench } from "lucide-react";
import { useEffect, useRef, useState, type ReactNode } from "react";
import { api, errorText, type SetupEvent, type SettingsView, type StackStatus } from "../lib/api";
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
      toast("info", component === "all" ? "Auto-setup started. Sign-in windows open where needed." : `Working on ${component}`);
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
      <div className="mb-5">
        <ConnectCard />
      </div>
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
          <Button variant="primary" size="lg" busy={activeJob !== null} icon={<Sparkles className="size-4" />} onClick={() => repair("all")}>
            Auto-Setup and Repair Everything
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
