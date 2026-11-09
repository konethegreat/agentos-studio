import { CalendarClock, Loader2, RefreshCw, ShieldCheck, Trash2 } from "lucide-react";
import { useCallback, useEffect, useState } from "react";
import { ApprovalDeclined, useGuarded } from "./ApprovalProvider";
import { useToast } from "./Toasts";
import { api, errorText, type QualityChange, type QualityReport } from "../lib/api";
import { studio, type CronJob } from "../lib/studio";

const WEEKLY_JOB = "agentos-memory-quality";

function Action({ change }: { change: QualityChange }) {
  if (change.written) return <span className="text-emerald-300">superseded</span>;
  if (change.protected) return <span className="text-amber-300">reported only (edited recently)</span>;
  return <span className="text-slate-400">would supersede</span>;
}

function Change({ change }: { change: QualityChange }) {
  return (
    <li className="px-4 py-2">
      <div className="flex items-center gap-2">
        <span
          className={`shrink-0 rounded-full px-1.5 py-0.5 text-2xs font-medium ring-1 ${
            change.kind === "stale"
              ? "bg-amber-500/10 text-amber-300 ring-amber-500/25"
              : "bg-rose-500/10 text-rose-300 ring-rose-500/25"
          }`}
        >
          {change.kind}
        </span>
        <span className="truncate font-mono text-caption text-slate-300" title={change.path}>
          {change.note}
        </span>
        <span className="ml-auto shrink-0 text-2xs">
          <Action change={change} />
        </span>
      </div>
      <div className="mt-0.5 text-2xs text-slate-500">
        {change.reason}
        {change.superseded_by ? ` Kept: ${change.superseded_by}.` : ""}
      </div>
    </li>
  );
}

/** Stale references and contradictory notes. A note edited in the last seven days is reported and
 *  left alone; nothing is ever deleted. The weekly job runs the same check through the exe. */
export function MemoryQualityPanel({ onChanged }: { onChanged?: () => void }) {
  const toast = useToast();
  const guarded = useGuarded();
  const [report, setReport] = useState<QualityReport | null>(null);
  const [jobs, setJobs] = useState<CronJob[] | null>(null);
  const [busy, setBusy] = useState<string | null>(null);

  const load = useCallback(() => {
    void api.memoryQualityLast().then(setReport).catch(() => setReport(null));
    void studio.hermes
      .cronJobs()
      .then(setJobs)
      .catch(() => setJobs([]));
  }, []);

  useEffect(() => {
    load();
  }, [load]);

  const weekly = jobs?.find((j) => j.name === WEEKLY_JOB) ?? null;

  async function run() {
    setBusy("run");
    try {
      const r = await api.memoryQualityRun(false);
      setReport(r);
      const parts = [
        `${r.stale.length} stale`,
        `${r.contradictions.length} contradictory`,
        `${r.stale.filter((c) => c.written).length + r.contradictions.filter((c) => c.written).length} superseded`,
      ];
      if (r.dropped) toast("error", `The contradiction answer was dropped: ${r.dropped}`);
      else toast(r.stale.length || r.contradictions.length ? "ok" : "info", `Quality check: ${parts.join(", ")}`);
      if (r.stale.some((c) => c.written) || r.contradictions.some((c) => c.written)) onChanged?.();
    } catch (e) {
      toast("error", errorText(e));
    } finally {
      setBusy(null);
    }
  }

  async function schedule() {
    setBusy("schedule");
    try {
      toast("ok", await guarded((a) => api.memoryQualitySchedule(a.approved ?? false)));
      load();
    } catch (e) {
      if (!(e instanceof ApprovalDeclined)) toast("error", errorText(e));
    } finally {
      setBusy(null);
    }
  }

  async function remove(id: string) {
    setBusy("remove");
    try {
      toast("ok", await guarded((a) => studio.hermes.cronAction(id, "remove", a)));
      load();
    } catch (e) {
      if (!(e instanceof ApprovalDeclined)) toast("error", errorText(e));
    } finally {
      setBusy(null);
    }
  }

  const changes = report ? [...report.stale, ...report.contradictions] : [];
  const superseded = changes.filter((c) => c.written).length;

  return (
    <div className="max-h-72 shrink-0 overflow-y-auto border-t border-line">
      <div className="flex items-center gap-2 border-b border-line px-4 py-2 text-small font-medium text-slate-400">
        <ShieldCheck className="size-3.5 text-sky-300" />
        Quality
        <span className="text-caption font-normal text-slate-500">stale references and contradictions</span>
        <button
          onClick={() => void run()}
          disabled={busy !== null}
          className="ml-auto inline-flex h-7 items-center gap-1.5 rounded-lg px-2 text-xs text-slate-300 ring-1 ring-line hover:bg-white/[0.04] disabled:opacity-50"
        >
          {busy === "run" ? <Loader2 className="size-3.5 animate-spin" /> : <ShieldCheck className="size-3.5" />}
          Run check
        </button>
        <button
          onClick={load}
          disabled={busy !== null}
          aria-label="Reload the quality report"
          title="Reload the quality report"
          className="inline-flex size-7 items-center justify-center rounded-lg text-slate-400 ring-1 ring-line hover:bg-white/[0.04] disabled:opacity-50"
        >
          <RefreshCw className="size-3.5" />
        </button>
      </div>

      <div className="flex flex-wrap items-center gap-2 border-b border-line/60 px-4 py-2 text-caption text-slate-500">
        <span>
          Last run {report ? new Date(report.ran_at).toLocaleString() : "never"}
          {report?.dry_run ? " (dry run)" : ""}
        </span>
        {report && (
          <span className="text-slate-400">
            {report.scanned} notes scanned, {report.stale.length} stale, {report.contradictions.length} contradictory,{" "}
            {superseded} superseded
          </span>
        )}
        {weekly ? (
          <span className="ml-auto inline-flex items-center gap-2 text-slate-400">
            <CalendarClock className="size-3.5 text-sky-300" />
            Weekly job {weekly.state ?? (weekly.enabled ? "active" : "off")}
            <button
              onClick={() => void remove(weekly.id)}
              disabled={busy !== null}
              aria-label="Remove the weekly quality job"
              title="Remove the weekly quality job"
              className="inline-flex size-6 items-center justify-center rounded-md text-slate-400 ring-1 ring-line hover:bg-white/[0.04] disabled:opacity-50"
            >
              {busy === "remove" ? <Loader2 className="size-3 animate-spin" /> : <Trash2 className="size-3" />}
            </button>
          </span>
        ) : (
          <button
            onClick={() => void schedule()}
            disabled={busy !== null || jobs === null}
            title="Create a Hermes cron job that runs this check every Monday at 06:00 without a model turn"
            className="ml-auto inline-flex h-7 items-center gap-1.5 rounded-lg px-2 text-xs text-slate-300 ring-1 ring-line hover:bg-white/[0.04] disabled:opacity-50"
          >
            {busy === "schedule" ? <Loader2 className="size-3.5 animate-spin" /> : <CalendarClock className="size-3.5" />}
            Schedule weekly check
          </button>
        )}
      </div>

      {report?.dropped && (
        <div className="border-b border-line/60 px-4 py-2 text-xs text-amber-200">
          The contradiction answer was dropped this run: {report.dropped}
        </div>
      )}

      {!report ? (
        <div className="px-4 py-3 text-xs text-slate-500">
          No quality run yet. A check reads notes that name a repository in their frontmatter.
        </div>
      ) : changes.length === 0 ? (
        <div className="px-4 py-3 text-xs text-slate-500">
          Nothing to change: no stale reference and no contradictory pair was found.
        </div>
      ) : (
        <ul className="divide-y divide-line/60">
          {changes.map((c, i) => (
            <Change key={`${c.path}-${i}`} change={c} />
          ))}
        </ul>
      )}
    </div>
  );
}
