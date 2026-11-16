import { TrendingUp, RefreshCw } from "lucide-react";
import { useCallback, useEffect, useState } from "react";
import { api, errorText, type RepoProof } from "../lib/api";

function pct(v: number) {
  return `${Math.round(v * 100)}%`;
}

/** Beyond B3.4: first-attempt approval rate and retries per repository, before and after each playbook. */
export function ProofOfLearningPanel() {
  const [rows, setRows] = useState<RepoProof[] | null>(null);
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(() => {
    setError(null);
    void api
      .playbookProof()
      .then(setRows)
      .catch((e) => setError(errorText(e)));
  }, []);

  useEffect(() => {
    load();
  }, [load]);

  return (
    <div className="max-h-64 shrink-0 overflow-y-auto border-t border-line">
      <div className="flex items-center gap-2 border-b border-line px-4 py-2 text-small font-medium text-slate-400">
        <TrendingUp className="size-3.5 text-emerald-300" />
        Proof of learning
        <span className="text-caption font-normal text-slate-500">
          first-attempt approval and retries before vs after playbook
        </span>
        <button
          onClick={load}
          aria-label="Reload proof of learning"
          title="Reload proof of learning"
          className="ml-auto inline-flex size-7 items-center justify-center rounded-lg text-slate-400 ring-1 ring-line hover:bg-white/[0.04]"
        >
          <RefreshCw className="size-3.5" />
        </button>
      </div>
      {error ? (
        <div className="px-4 py-3 text-xs text-amber-200">{error}</div>
      ) : rows === null ? (
        <div className="px-4 py-3 text-xs text-slate-500">Measuring outcomes...</div>
      ) : rows.length === 0 ? (
        <div className="px-4 py-3 text-xs text-slate-500">
          No decided missions yet.
        </div>
      ) : (
        <ul className="divide-y divide-line/60">
          {rows.map((r) => (
            <li key={r.repo} className="space-y-1.5 px-4 py-2.5">
              <div className="flex items-center gap-2 text-small">
                <span className="font-medium text-slate-200">{r.repo}</span>
                <span className="text-2xs text-slate-500">{r.lessons} active lessons</span>
                <span className="ml-auto text-2xs text-slate-400">
                  {r.before.missions} before, {r.after.missions} after
                </span>
              </div>
              <div className="grid grid-cols-2 gap-3 text-2xs text-slate-400">
                <div>
                  <div className="flex justify-between">
                    <span>Before playbook</span>
                    <span>{pct(r.before.first_attempt_rate)} ({r.before.avg_retries.toFixed(1)} retries)</span>
                  </div>
                  <div className="mt-1 h-1.5 overflow-hidden rounded-full bg-slate-800">
                    <div
                      className="h-full rounded-full bg-slate-500"
                      style={{ width: `${Math.round(r.before.first_attempt_rate * 100)}%` }}
                    />
                  </div>
                </div>
                <div>
                  <div className="flex justify-between">
                    <span>After playbook</span>
                    <span>{pct(r.after.first_attempt_rate)} ({r.after.avg_retries.toFixed(1)} retries)</span>
                  </div>
                  <div className="mt-1 h-1.5 overflow-hidden rounded-full bg-slate-800">
                    <div
                      className="h-full rounded-full bg-emerald-400"
                      style={{ width: `${Math.round(r.after.first_attempt_rate * 100)}%` }}
                    />
                  </div>
                </div>
              </div>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
