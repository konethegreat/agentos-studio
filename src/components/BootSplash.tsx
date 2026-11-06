import { AlertTriangle, Check, CircleDot, Loader2, Wrench, X } from "lucide-react";
import { useEffect, useState, type ReactNode } from "react";
import type { BootReport, BootStep } from "../lib/api";

const TONE: Record<string, { icon: ReactNode; text: string }> = {
  ok: { icon: <Check className="size-3.5" />, text: "text-emerald-300" },
  repaired: { icon: <Wrench className="size-3.5" />, text: "text-sky-300" },
  warn: { icon: <AlertTriangle className="size-3.5" />, text: "text-amber-300" },
  failed: { icon: <X className="size-3.5" />, text: "text-red-300" },
  skipped: { icon: <CircleDot className="size-3.5" />, text: "text-slate-500" },
};

/**
 * Startup screen. Renders the staged checks as they run, so a slow launch looks like work
 * rather than a hang, and surfaces anything broken before the user starts typing.
 */
export function BootSplash({
  steps,
  report,
  done,
  onContinue,
  onDiagnostics,
  onFix,
}: {
  steps: BootStep[];
  report: BootReport | null;
  done: boolean;
  onContinue: () => void;
  onDiagnostics: () => void;
  onFix: (view: string) => void;
}) {
  const problems = steps.filter((s) => s.status === "warn" || s.status === "failed");
  const repaired = steps.filter((s) => s.status === "repaired");
  const [slow, setSlow] = useState(false);

  useEffect(() => {
    if (done) return;
    const t = window.setTimeout(() => setSlow(true), 12_000);
    return () => window.clearTimeout(t);
  }, [done]);

  // A clean boot gets out of the way on its own; anything with a problem asks for a decision.
  useEffect(() => {
    if (!done || problems.length > 0) return;
    const t = window.setTimeout(onContinue, repaired.length > 0 ? 1600 : 700);
    return () => window.clearTimeout(t);
  }, [done, problems.length, repaired.length, onContinue]);

  const total = steps.length || 8;

  return (
    <div className="fixed inset-0 z-[60] flex flex-col items-center justify-center bg-canvas px-6">
      <div className="w-full max-w-xl">
        <div className="mb-8 flex items-center gap-3">
          <img src="/icon.png" alt="" className="size-10 rounded-xl" />
          <div>
            <div className="text-base font-semibold tracking-tight text-slate-100">AgentOS Studio</div>
            <div className="text-[11.5px] text-slate-500">{report?.app_version ? `v${report.app_version}` : "starting"}</div>
          </div>
        </div>

        <div className="mb-5 h-1 overflow-hidden rounded-full bg-white/5">
          <div
            className="h-full rounded-full bg-emerald-400/80 transition-[width] duration-500"
            style={{ width: `${Math.min(100, (steps.length / total) * 100)}%` }}
          />
        </div>

        <div className="space-y-1.5">
          {steps.map((s) => {
            const tone = TONE[s.status] ?? TONE.skipped;
            return (
              <div key={s.id} className="flex items-start gap-2.5 text-[12.5px]">
                <span className={`mt-0.5 shrink-0 ${tone.text}`}>{tone.icon}</span>
                <div className="min-w-0 flex-1">
                  <span className="text-slate-200">{s.label}</span>
                  <span className="ml-2 text-slate-500">{s.detail}</span>
                </div>
                <span className="shrink-0 tabular-nums text-[11px] text-slate-600">{s.ms} ms</span>
              </div>
            );
          })}
          {!done && (
            <div className="flex items-center gap-2.5 pt-1 text-[12.5px] text-slate-400">
              <Loader2 className="size-3.5 animate-spin text-slate-500" />
              Checking…
            </div>
          )}
        </div>

        {report?.previous_crash && (
          <div className="mt-5 rounded-lg bg-amber-500/10 px-3 py-2 text-[11.5px] text-amber-200 ring-1 ring-amber-500/25">
            The last session ended with an error: {report.previous_crash}
          </div>
        )}

        {done && problems.length > 0 && (
          <div className="mt-6 space-y-2">
            <div className="rounded-lg bg-amber-500/10 px-3 py-2 text-[12px] text-amber-200 ring-1 ring-amber-500/25">
              {problems.length} item{problems.length > 1 ? "s" : ""} need attention. You can continue and fix them later.
            </div>
            <div className="flex flex-wrap gap-2">
              {problems
                .filter((p) => p.fix_view)
                .slice(0, 3)
                .map((p) => (
                  <button
                    key={p.id}
                    onClick={() => onFix(p.fix_view as string)}
                    className="inline-flex h-8 items-center gap-1.5 rounded-lg bg-white/[0.06] px-3 text-xs text-slate-200 ring-1 ring-white/10 hover:bg-white/[0.1]"
                  >
                    <Wrench className="size-3.5" /> Fix {p.label.toLowerCase()}
                  </button>
                ))}
              <button onClick={onContinue} className="inline-flex h-8 items-center rounded-lg bg-emerald-500 px-3 text-xs font-semibold text-emerald-950 hover:bg-emerald-400">
                Continue anyway
              </button>
              <button onClick={onDiagnostics} className="inline-flex h-8 items-center rounded-lg px-3 text-xs text-slate-400 hover:bg-white/5 hover:text-slate-200">
                Diagnostics
              </button>
            </div>
          </div>
        )}

        {(slow || done) && problems.length === 0 && (
          <div className="mt-6 flex gap-2">
            {!done && (
              <button onClick={onContinue} className="inline-flex h-8 items-center rounded-lg px-3 text-xs text-slate-400 hover:bg-white/5 hover:text-slate-200">
                Continue without waiting
              </button>
            )}
            <button onClick={onDiagnostics} className="inline-flex h-8 items-center rounded-lg px-3 text-xs text-slate-400 hover:bg-white/5 hover:text-slate-200">
              Diagnostics
            </button>
          </div>
        )}

        <div className="mt-8 text-[11px] text-slate-600">
          Checks run on every launch. Only safe repairs happen automatically; anything that changes another tool's configuration is shown here for you to decide.
        </div>
      </div>
    </div>
  );
}
