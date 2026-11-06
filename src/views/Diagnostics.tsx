import { AlertTriangle, Check, CircleDot, FileText, FolderOpen, Loader2, Play, Wrench, X } from "lucide-react";
import { useCallback, useEffect, useState, type ReactNode } from "react";
import { api, errorText, type BootReport, type BootStep } from "../lib/api";
import { useToast } from "../components/Toasts";
import { Badge, Button, Card, Empty } from "../components/ui";

const TONE: Record<string, { icon: ReactNode; text: string; badge: string }> = {
  ok: { icon: <Check className="size-3.5" />, text: "text-emerald-300", badge: "bg-emerald-500/10 text-emerald-300 ring-emerald-500/25" },
  repaired: { icon: <Wrench className="size-3.5" />, text: "text-sky-300", badge: "bg-sky-500/10 text-sky-300 ring-sky-500/25" },
  warn: { icon: <AlertTriangle className="size-3.5" />, text: "text-amber-300", badge: "bg-amber-500/10 text-amber-300 ring-amber-500/25" },
  failed: { icon: <X className="size-3.5" />, text: "text-red-300", badge: "bg-red-500/10 text-red-300 ring-red-500/25" },
  skipped: { icon: <CircleDot className="size-3.5" />, text: "text-slate-500", badge: "bg-white/5 text-slate-400 ring-white/10" },
};

/**
 * Diagnostics: the startup report, the crash log, and the two log files this app writes.
 * This is the screen that answers "why did it not start" without a terminal.
 */
export function Diagnostics({ onFix }: { onFix: (view: string) => void }) {
  const toast = useToast();
  const [report, setReport] = useState<BootReport | null>(null);
  const [running, setRunning] = useState(false);
  const [logName, setLogName] = useState<"boot.log" | "crash.log">("boot.log");
  const [log, setLog] = useState<string>("");
  const [loadingLog, setLoadingLog] = useState(false);

  const loadLog = useCallback(
    async (name: "boot.log" | "crash.log") => {
      setLoadingLog(true);
      try {
        setLog(await api.bootReadLog(name));
      } catch (e) {
        setLog(`No ${name} yet. ${errorText(e)}`);
      } finally {
        setLoadingLog(false);
      }
    },
    [],
  );

  useEffect(() => {
    void api.bootSnapshot().then(setReport).catch((e) => toast("error", errorText(e)));
    void loadLog("boot.log");
  }, [loadLog, toast]);

  async function rerun() {
    setRunning(true);
    try {
      const r = await api.bootRun();
      setReport(r);
      toast(r.summary.failed > 0 ? "error" : r.summary.warn > 0 ? "info" : "ok", `Checks finished in ${r.elapsed_ms} ms`);
      void loadLog(logName);
    } catch (e) {
      toast("error", errorText(e));
    } finally {
      setRunning(false);
    }
  }

  async function openLogs() {
    try {
      toast("ok", `Opened ${await api.bootOpenLogs()}`);
    } catch (e) {
      toast("error", errorText(e));
    }
  }

  const steps: BootStep[] = report?.steps ?? [];
  const problems = steps.filter((s) => s.status === "warn" || s.status === "failed");

  return (
    <div className="flex h-full min-h-0 flex-col">
      <div className="flex items-end justify-between px-6 pt-5">
        <div>
          <h1 className="text-lg font-semibold text-slate-100">Diagnostics</h1>
          <p className="text-xs text-muted">
            What happened at startup, what was repaired, and the logs. No terminal needed.
          </p>
        </div>
        <div className="flex gap-2">
          <Button onClick={() => void rerun()} busy={running} icon={<Play className="size-4" />}>
            Re-run checks
          </Button>
          <Button variant="ghost" onClick={() => void openLogs()} icon={<FolderOpen className="size-4" />}>
            Logs folder
          </Button>
        </div>
      </div>

      <div className="min-h-0 flex-1 overflow-y-auto px-6 pb-8 pt-4">
        {!report && <Empty icon={<Loader2 className="size-5 animate-spin" />} title="No startup report yet" hint="Press Re-run checks" />}

        {report && (
          <>
            <Card className="mb-4 flex flex-wrap items-center gap-4 px-5 py-4">
              <div className="min-w-0 flex-1">
                <div className="text-[11px] uppercase tracking-wide text-slate-500">Last run</div>
                <div className="text-sm font-semibold text-slate-100">
                  v{report.app_version} in {report.elapsed_ms} ms
                </div>
                <div className="text-[11px] text-slate-500">
                  {report.started_at} · vault {report.vault}
                </div>
              </div>
              <div className="flex flex-wrap gap-1.5">
                <Badge className={TONE.ok.badge}>{report.summary.ok} ok</Badge>
                {report.summary.repaired > 0 && <Badge className={TONE.repaired.badge}>{report.summary.repaired} repaired</Badge>}
                {report.summary.warn > 0 && <Badge className={TONE.warn.badge}>{report.summary.warn} warning</Badge>}
                {report.summary.failed > 0 && <Badge className={TONE.failed.badge}>{report.summary.failed} failed</Badge>}
              </div>
            </Card>

            {report.previous_crash && (
              <Card className="mb-4 border-amber-500/30 px-5 py-3">
                <div className="text-[11px] font-semibold uppercase tracking-wide text-amber-300">Last session ended with an error</div>
                <div className="mt-1 font-mono text-[11.5px] leading-relaxed text-amber-100/80">{report.previous_crash}</div>
              </Card>
            )}

            <Card className="mb-4 overflow-hidden">
              <div className="border-b border-line px-5 py-2.5 text-[11px] font-semibold uppercase tracking-wide text-slate-500">Startup checks</div>
              <div className="divide-y divide-line/60">
                {steps.map((s) => {
                  const tone = TONE[s.status] ?? TONE.skipped;
                  return (
                    <div key={s.id} className="flex items-start gap-3 px-5 py-2.5">
                      <span className={`mt-0.5 shrink-0 ${tone.text}`}>{tone.icon}</span>
                      <div className="min-w-0 flex-1">
                        <div className="flex items-center gap-2">
                          <span className="text-[13px] text-slate-200">{s.label}</span>
                          <Badge className={tone.badge}>{s.status}</Badge>
                        </div>
                        <div className="mt-0.5 break-words text-[11.5px] leading-relaxed text-slate-500">{s.detail}</div>
                      </div>
                      <span className="shrink-0 tabular-nums text-[11px] text-slate-600">{s.ms} ms</span>
                      {s.fix_view && (
                        <Button size="sm" variant="ghost" onClick={() => onFix(s.fix_view as string)}>
                          Fix
                        </Button>
                      )}
                    </div>
                  );
                })}
              </div>
            </Card>

            {problems.length > 0 && (
              <div className="mb-4 rounded-lg bg-amber-500/10 px-4 py-3 text-[12px] text-amber-200 ring-1 ring-amber-500/25">
                Warnings never block startup. Safe repairs (folders, database, PATH, vault structure) run automatically; configuration, installs and sign-ins are left to you.
              </div>
            )}
          </>
        )}

        <Card className="overflow-hidden">
          <div className="flex items-center gap-2 border-b border-line px-4 py-2">
            <FileText className="size-3.5 text-slate-500" />
            {(["boot.log", "crash.log"] as const).map((n) => (
              <button
                key={n}
                onClick={() => {
                  setLogName(n);
                  void loadLog(n);
                }}
                className={`rounded-md px-2 py-1 font-mono text-[11px] ${logName === n ? "bg-white/[0.07] text-slate-100" : "text-slate-500 hover:text-slate-300"}`}
              >
                {n}
              </button>
            ))}
            <div className="flex-1" />
            {loadingLog && <Loader2 className="size-3.5 animate-spin text-slate-500" />}
            <button onClick={() => void loadLog(logName)} className="rounded-md px-2 py-1 text-[11px] text-slate-400 hover:bg-white/5 hover:text-slate-200">
              Reload
            </button>
          </div>
          <pre className="max-h-[22rem] overflow-auto whitespace-pre-wrap break-words px-4 py-3 font-mono text-[11px] leading-relaxed text-slate-400">
            {log || "Empty."}
          </pre>
        </Card>
      </div>
    </div>
  );
}
