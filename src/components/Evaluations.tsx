import { FlaskConical, Loader2, RefreshCw } from "lucide-react";
import { useCallback, useEffect, useMemo, useState } from "react";
import { api, errorText, type EvalSummary, type Mission } from "../lib/api";
import { useGuarded, ApprovalDeclined } from "./ApprovalProvider";
import { useToast } from "./Toasts";

const SHOWN = 6;
const fieldClass =
  "h-7 rounded-lg border border-line bg-canvas px-2 text-xs text-slate-100 outline-none focus:border-emerald-500/60";

function deltaStyle(score: number, original: number): string {
  if (score > original) return "bg-emerald-500/10 text-emerald-300 ring-emerald-500/25";
  if (score < original) return "bg-rose-500/10 text-rose-300 ring-rose-500/25";
  return "bg-slate-500/10 text-slate-300 ring-slate-500/25";
}

/** Replay a finished mission on another agent or model. Each replay runs in a throwaway
 *  worktree and writes one note under 03_Memory/evals; nothing is pushed or opened. */
export function EvaluationsPanel() {
  const toast = useToast();
  const guarded = useGuarded();
  const [missions, setMissions] = useState<Mission[] | null>(null);
  const [rows, setRows] = useState<EvalSummary[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [mission, setMission] = useState("");
  const [agent, setAgent] = useState("claude-code");
  const [model, setModel] = useState("");
  const [busy, setBusy] = useState(false);

  const load = useCallback(() => {
    setError(null);
    void api
      .missions()
      .then((all) => {
        const finished = all.filter(
          (m) => (m.status === "done" || m.status === "failed") && m.pr_url,
        );
        setMissions(finished);
        setMission((current) => current || finished[0]?.id || "");
      })
      .catch((e) => setError(errorText(e)));
    void api
      .evalList()
      .then(setRows)
      .catch((e) => setError(errorText(e)));
  }, []);

  useEffect(() => {
    load();
  }, [load]);

  const replayable = useMemo(() => missions ?? [], [missions]);

  async function replay() {
    if (!mission) return;
    setBusy(true);
    try {
      const r = await guarded((a) =>
        api.evalRun(mission, agent, model.trim() || undefined, a.approved),
      );
      toast(
        r.score >= r.original_score ? "ok" : "info",
        `${r.mission} on ${r.agent}: ${r.score}/100. ${r.comparison}`,
      );
      load();
    } catch (e) {
      if (!(e instanceof ApprovalDeclined)) toast("error", errorText(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="max-h-80 shrink-0 overflow-y-auto border-t border-line">
      <div className="flex items-center gap-2 border-b border-line px-4 py-2 text-small font-medium text-slate-400">
        <FlaskConical className="size-3.5 text-sky-300" />
        Evaluations
        <span className="text-caption font-normal text-slate-500">replays of finished missions</span>
        <button
          onClick={load}
          disabled={busy}
          aria-label="Reload evaluations"
          title="Reload evaluations"
          className="ml-auto inline-flex size-7 items-center justify-center rounded-lg text-slate-400 ring-1 ring-line hover:bg-white/[0.04] disabled:opacity-50"
        >
          <RefreshCw className="size-3.5" />
        </button>
      </div>
      <div className="flex flex-wrap items-center gap-1.5 border-b border-line px-4 py-2">
        <select
          aria-label="Mission to replay"
          value={mission}
          onChange={(e) => setMission(e.target.value)}
          disabled={busy || replayable.length === 0}
          className={`${fieldClass} max-w-52`}
        >
          {replayable.length === 0 ? (
            <option value="">No finished mission with a pull request</option>
          ) : (
            replayable.map((m) => (
              <option key={m.id} value={m.id}>
                {m.id} {m.goal}
              </option>
            ))
          )}
        </select>
        <select
          aria-label="Agent for the replay"
          value={agent}
          onChange={(e) => setAgent(e.target.value)}
          disabled={busy}
          className={fieldClass}
        >
          <option value="claude-code">claude-code</option>
          <option value="hermes">hermes</option>
        </select>
        <input
          aria-label="Model for the replay"
          value={model}
          onChange={(e) => setModel(e.target.value)}
          disabled={busy}
          placeholder="model (optional)"
          className={`${fieldClass} w-40`}
        />
        <button
          onClick={() => void replay()}
          disabled={busy || !mission}
          title="Replay in a detached worktree at the pull request's base commit, then test and review it"
          className="ml-auto inline-flex h-7 shrink-0 items-center gap-1.5 rounded-lg px-2 text-xs text-slate-300 ring-1 ring-line hover:bg-white/[0.04] disabled:opacity-50"
        >
          {busy ? <Loader2 className="size-3.5 animate-spin" /> : <FlaskConical className="size-3.5" />}
          Replay
        </button>
      </div>
      {error ? (
        <div className="px-4 py-3 text-xs text-amber-200">{error}</div>
      ) : rows === null ? (
        <div className="px-4 py-3 text-xs text-slate-500">Reading evaluations...</div>
      ) : rows.length === 0 ? (
        <div className="px-4 py-3 text-xs text-slate-500">
          No replay yet. Pick a finished mission above; the note lands in 03_Memory/evals.
        </div>
      ) : (
        <ul className="divide-y divide-line/60">
          {rows.slice(0, SHOWN).map((r) => (
            <li key={r.file} className="px-4 py-2">
              <div className="flex items-center gap-2">
                <span className="shrink-0 font-mono text-caption text-slate-500">{r.mission}</span>
                <span className="shrink-0 text-small text-slate-300">
                  {r.agent}
                  {r.model && r.model !== "default" ? ` (${r.model})` : ""}
                </span>
                <span
                  className={`shrink-0 rounded-full px-1.5 py-0.5 text-2xs font-medium ring-1 ${deltaStyle(r.score, r.original_score)}`}
                  title="Replay score against the original run"
                >
                  {r.score} vs {r.original_score}
                </span>
                <span className="shrink-0 text-caption text-slate-500">
                  {r.verdict}, tests {r.tests}
                </span>
                <span className="ml-auto shrink-0 text-2xs text-slate-500">{r.date}</span>
              </div>
              <div className="mt-0.5 truncate text-caption text-slate-500" title={r.file}>
                {r.comparison}
              </div>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
