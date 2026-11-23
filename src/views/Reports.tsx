import { Activity, ArrowUpRight, FileText, Loader2, RefreshCw } from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Badge, Button, Card, Empty, ExternalLink, inputClass } from "../components/ui";
import { errorText } from "../lib/api";
import { studio, type FleetReport, type RepoSummary } from "../lib/studio";

const KIND_LABEL: Record<string, string> = {
  "mission.started": "Started",
  "mission.routed": "Routed",
  "mission.reviewed": "For review",
  "mission.merged": "Merged",
  "mission.blocked": "Stopped",
  "mission.unstuck": "Unstuck",
  "escalation.auto_answered": "Answered",
  "autonomy.pass": "Autonomy",
  "digest.sent": "Digest",
  "fleet_report.sent": "Fleet report",
  "update.applied": "Update",
};

const KIND_TONE: Record<string, string> = {
  "mission.merged": "bg-emerald-500/10 text-emerald-300 ring-emerald-500/30",
  "mission.blocked": "bg-red-500/10 text-red-300 ring-red-500/30",
  "mission.unstuck": "bg-amber-500/10 text-amber-300 ring-amber-500/30",
  "escalation.auto_answered": "bg-sky-500/10 text-sky-300 ring-sky-500/30",
  "update.applied": "bg-sky-500/10 text-sky-300 ring-sky-500/30",
};

const NEUTRAL = "bg-white/5 text-slate-300 ring-white/10";

function when(ts: number) {
  if (!ts) return "";
  const d = new Date(ts * 1000);
  const same = d.toDateString() === new Date().toDateString();
  if (same) return `Today ${d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })}`;
  return d.toLocaleString([], { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" });
}

/**
 * The report feed: every event the orchestrator, scheduler and updater wrote, newest first.
 * It is read-only by design. Nothing in it asks the owner to do anything.
 */
export function Reports() {
  const [rows, setRows] = useState<FleetReport[]>([]);
  const [summary, setSummary] = useState<RepoSummary[]>([]);
  const [sinceCount, setSinceCount] = useState<number | null>(null);
  const [kind, setKind] = useState("");
  const [repo, setRepo] = useState("");
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const firstLoad = useRef(true);

  const load = useCallback(async () => {
    try {
      const [list, sums, unread] = await Promise.all([studio.reports.list(null, null, 300), studio.reports.summary(24), studio.reports.unread()]);
      setRows(list);
      setSummary(sums);
      setError(null);
      if (firstLoad.current) {
        setSinceCount(unread);
        firstLoad.current = false;
      }
      const max = list.reduce((m, r) => Math.max(m, r.id), 0);
      if (max > 0) await studio.reports.markRead(max);
    } catch (e) {
      setError(errorText(e));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load();
    const t = window.setInterval(() => void load(), 30_000);
    return () => window.clearInterval(t);
  }, [load]);

  const kinds = useMemo(() => Array.from(new Set(rows.map((r) => r.kind))).sort(), [rows]);
  const repos = useMemo(() => Array.from(new Set(rows.map((r) => r.repo).filter(Boolean))).sort(), [rows]);
  const shown = useMemo(() => rows.filter((r) => (!kind || r.kind === kind) && (!repo || r.repo === repo)), [rows, kind, repo]);

  const headline =
    sinceCount === null
      ? "Reading the feed."
      : sinceCount > 0
        ? `${sinceCount} report${sinceCount === 1 ? "" : "s"} arrived while you were away. Nothing here needs you.`
        : "You are caught up. New work appears here as it happens.";

  return (
    <div className="h-full overflow-y-auto">
      <div className="mx-auto max-w-4xl space-y-5 px-6 py-6">
        <div className="flex items-center gap-3">
          <div className="flex-1">
            <h1 className="text-lg font-semibold text-slate-100">Reports</h1>
            <p className="text-xs text-slate-500">
              What the orchestrator and the fleet did in every repository, newest first. This page is a record, not a queue: reading it is all you ever do here.
            </p>
          </div>
          <Button icon={loading ? <Loader2 className="size-4 animate-spin" /> : <RefreshCw className="size-4" />} disabled={loading} onClick={() => void load()}>
            Refresh
          </Button>
        </div>

        <Card className="p-5">
          <div className="flex items-center gap-2">
            <Activity className="size-4 text-emerald-300" />
            <h2 className="text-sm font-semibold text-slate-100">Since you last looked</h2>
            <span className="text-label text-slate-500">counted over the last 24 hours</span>
          </div>
          <p className="mt-1 text-xs text-slate-400">{headline}</p>
          {summary.length > 0 && (
            <div className="mt-3 grid grid-cols-1 gap-2 sm:grid-cols-2 lg:grid-cols-3">
              {summary.map((s) => (
                <button
                  key={s.repo || "fleet"}
                  onClick={() => setRepo(s.repo)}
                  title={`Only reports from ${s.repo || "the fleet itself"}`}
                  className="flex items-baseline gap-2 rounded-lg border border-line bg-canvas/60 px-3 py-2 text-left text-xs hover:border-slate-500/50"
                >
                  <span className="min-w-0 flex-1 truncate text-slate-200">{s.repo || "Fleet"}</span>
                  <span className="font-mono text-slate-400">{s.count}</span>
                  <span className="shrink-0 text-label text-slate-600">{when(s.last_at)}</span>
                </button>
              ))}
            </div>
          )}
        </Card>

        <div className="flex flex-wrap items-center gap-2">
          <select value={kind} onChange={(e) => setKind(e.target.value)} className={`${inputClass} !w-48`} aria-label="Kind of report">
            <option value="">Every kind</option>
            {kinds.map((k) => (
              <option key={k} value={k}>
                {KIND_LABEL[k] ?? k}
              </option>
            ))}
          </select>
          <select value={repo} onChange={(e) => setRepo(e.target.value)} className={`${inputClass} !w-48`} aria-label="Repository">
            <option value="">Every repository</option>
            {repos.map((r) => (
              <option key={r} value={r}>
                {r}
              </option>
            ))}
          </select>
          <div className="flex-1" />
          <span className="text-label text-slate-600">
            {shown.length} of {rows.length} reports
          </span>
        </div>

        {error && <Card className="border-amber-500/30 p-4 text-xs text-amber-300">Could not read the feed: {error}</Card>}

        <Card className="p-0">
          {shown.length === 0 ? (
            <div className="px-4 py-8">
              <Empty icon={<FileText className="size-6" />} title="No reports yet" hint="Missions, merges, answered questions, digests and updates land here as they happen." />
            </div>
          ) : (
            shown.map((r) => (
              <div key={r.id} className="flex items-start gap-3 border-b border-line px-4 py-3 last:border-0">
                <span className="w-28 shrink-0 pt-0.5 text-label text-slate-500">{when(r.at)}</span>
                <span className="w-24 shrink-0 pt-0.5">
                  <Badge className={KIND_TONE[r.kind] ?? NEUTRAL}>{KIND_LABEL[r.kind] ?? r.kind}</Badge>
                </span>
                <div className="min-w-0 flex-1">
                  <div className="flex items-center gap-2 text-xs">
                    <span className="min-w-0 truncate font-medium text-slate-200">{r.title}</span>
                    {r.repo && <span className="shrink-0 rounded bg-white/5 px-1.5 font-mono text-label text-slate-400">{r.repo}</span>}
                  </div>
                  {r.body && <p className="mt-0.5 line-clamp-2 whitespace-pre-wrap text-label leading-relaxed text-slate-500">{r.body}</p>}
                </div>
                {r.reference.startsWith("http") && (
                  <ExternalLink href={r.reference} className="mt-0.5 inline-flex shrink-0 items-center gap-1 text-label no-underline">
                    Open <ArrowUpRight className="size-3" />
                  </ExternalLink>
                )}
              </div>
            ))
          )}
        </Card>
      </div>
    </div>
  );
}
