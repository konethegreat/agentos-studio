import { useCallback, useEffect, useState } from "react";
import { History, Loader2, RefreshCw } from "lucide-react";
import { api, errorText, type BenchStatus, type ModelChanges } from "../lib/api";
import { useToast } from "./Toasts";

const FLAT = (p: string) => p.startsWith("claude-subscription") || p.startsWith("antigravity-");
const btn =
  "inline-flex h-7 items-center gap-1 rounded-lg border border-line px-2 text-xs text-slate-200 hover:bg-white/5 disabled:opacity-50";

/**
 * The time machine (docs/BEYOND.md, B4): approved missions are the benchmark set, and a new model
 * on a flat subscription is replayed on them the week it appears. Replays run one at a time, at
 * most a few a day, in throwaway worktrees; their eval notes feed the router's scorecards.
 */
export function TimeMachinePanel() {
  const toast = useToast();
  const [s, setS] = useState<BenchStatus | null>(null);
  const [changes, setChanges] = useState<ModelChanges | null>(null);
  const [busy, setBusy] = useState("");

  const load = useCallback(async () => {
    try {
      const [b, c] = await Promise.all([api.benchStatus(), api.modelChanges().catch(() => null)]);
      setS(b);
      setChanges(c);
    } catch (e) {
      toast("error", errorText(e));
    }
  }, [toast]);

  useEffect(() => {
    void load();
  }, [load]);

  async function measure(provider: string, model: string) {
    setBusy(`${provider}/${model}`);
    try {
      const jobs = await api.benchMeasure(provider, model);
      toast("ok", jobs.length ? `Queued ${jobs.length} replays on ${model}` : `${model} is already measured or queued`);
      await load();
    } catch (e) {
      toast("error", errorText(e));
    } finally {
      setBusy("");
    }
  }

  const fresh = (changes?.new ?? []).filter((n) => FLAT(n.provider) && !n.model.includes("["));

  return (
    <div className="shrink-0 border-t border-line">
      <div className="flex items-center gap-2 border-b border-line px-4 py-2 text-small font-medium text-slate-400">
        <History className="size-3.5 text-sky-300" /> Time machine
        <span className="font-normal text-slate-500">
          {s ? `${s.cases.length} benchmark missions, ${s.queue.length} queued, ${s.today} of ${s.per_day} today` : "reading"}
          {s?.running ? ", one replaying now" : ""}
        </span>
        <button type="button" className={`${btn} ml-auto`} onClick={() => void load()} aria-label="Refresh the time machine">
          <RefreshCw className="size-3" />
        </button>
      </div>
      <div className="space-y-2 px-4 py-2 text-small">
        <p className="text-label text-slate-500">
          Every approved mission is a benchmark. A new model on your Claude or Antigravity subscription is replayed on
          three of them, in a sandbox that never pushes, and scored on the tests and a second agent&apos;s review.
        </p>
        {fresh.length > 0 && (
          <div className="space-y-1">
            <div className="text-label text-slate-400">New this week</div>
            {fresh.map((n) => (
              <div key={`${n.provider}/${n.model}`} className="flex flex-wrap items-center gap-2">
                <span className="font-mono text-slate-200">{n.model}</span>
                <span className="text-label text-slate-500">{n.provider}</span>
                <button
                  type="button"
                  className={`${btn} ml-auto`}
                  disabled={busy !== ""}
                  onClick={() => void measure(n.provider, n.model)}
                >
                  {busy === `${n.provider}/${n.model}` && <Loader2 className="size-3 animate-spin" />} Measure
                </button>
              </div>
            ))}
          </div>
        )}
        {s && s.queue.length > 0 && (
          <div className="space-y-0.5">
            <div className="flex items-center gap-2 text-label text-slate-400">
              Queued
              <button
                type="button"
                className={`${btn} ml-auto`}
                onClick={() =>
                  void api
                    .benchClear()
                    .then(setS)
                    .catch((e) => toast("error", errorText(e)))
                }
              >
                Clear the queue
              </button>
            </div>
            {s.queue.slice(0, 8).map((j) => (
              <div key={`${j.mission}-${j.model}`} className="flex gap-2 text-label text-slate-500">
                <span className="font-mono text-slate-300">{j.mission}</span>
                <span className="font-mono">{j.model}</span>
                <span className="truncate">{j.reason}</span>
              </div>
            ))}
          </div>
        )}
      </div>
    </div>
  );
}
