import { GraduationCap, Loader2, RefreshCw } from "lucide-react";
import { useCallback, useEffect, useMemo, useState } from "react";
import { api, errorText, type Mission } from "../lib/api";
import { useToast } from "./Toasts";

const SHOWN = 5;

const STATUS_STYLE: Record<string, string> = {
  done: "bg-emerald-500/10 text-emerald-300 ring-emerald-500/25",
  failed: "bg-rose-500/10 text-rose-300 ring-rose-500/25",
};

/** Finished missions, with the manual entry point into the learning loop. The loop writes
 *  vault notes only; it never changes a mission or runs the reviewer. */
export function LearningPanel({ onWrote }: { onWrote?: () => void }) {
  const toast = useToast();
  const [missions, setMissions] = useState<Mission[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);

  const load = useCallback(() => {
    setError(null);
    void api
      .missions()
      .then(setMissions)
      .catch((e) => setError(errorText(e)));
  }, []);

  useEffect(() => {
    load();
  }, [load]);

  const finished = useMemo(
    () =>
      (missions ?? [])
        .filter((m) => m.status === "done" || m.status === "failed")
        .sort((a, b) => b.updated_at - a.updated_at)
        .slice(0, SHOWN),
    [missions],
  );

  async function extract(m: Mission) {
    setBusy(m.id);
    try {
      const r = await api.extractLessons(m.id);
      if (r.skipped) {
        toast("info", `${m.id} was already extracted; nothing was written.`);
      } else if (r.dropped) {
        toast("error", `${m.id}: the model answer was dropped. ${r.dropped}`);
      } else {
        const parts = [`${r.notes.length} lesson note${r.notes.length === 1 ? "" : "s"} written`];
        if (r.conventions.length) {
          parts.push(`${r.conventions.length} convention note${r.conventions.length === 1 ? "" : "s"} updated`);
        }
        toast(r.notes.length || r.conventions.length ? "ok" : "info", `${m.id}: ${parts.join("; ")}`);
        if (r.notes.length || r.conventions.length) onWrote?.();
      }
    } catch (e) {
      toast("error", errorText(e));
    } finally {
      setBusy(null);
    }
  }

  return (
    <div className="max-h-60 shrink-0 overflow-y-auto border-t border-line">
      <div className="flex items-center gap-2 border-b border-line px-4 py-2 text-small font-medium text-slate-400">
        <GraduationCap className="size-3.5 text-emerald-300" />
        Learning
        <span className="text-caption font-normal text-slate-500">lessons from finished missions</span>
        <button
          onClick={load}
          disabled={busy !== null}
          aria-label="Reload finished missions"
          title="Reload finished missions"
          className="ml-auto inline-flex size-7 items-center justify-center rounded-lg text-slate-400 ring-1 ring-line hover:bg-white/[0.04] disabled:opacity-50"
        >
          <RefreshCw className="size-3.5" />
        </button>
      </div>
      {error ? (
        <div className="px-4 py-3 text-xs text-amber-200">{error}</div>
      ) : missions === null ? (
        <div className="px-4 py-3 text-xs text-slate-500">Reading missions...</div>
      ) : finished.length === 0 ? (
        <div className="px-4 py-3 text-xs text-slate-500">
          No finished mission yet. A mission appears here once it is done or failed.
        </div>
      ) : (
        <ul className="divide-y divide-line/60">
          {finished.map((m) => (
            <li key={m.id} className="flex items-center gap-2 px-4 py-2">
              <span className="shrink-0 font-mono text-caption text-slate-500">{m.id}</span>
              <span className="truncate text-small text-slate-300" title={m.goal}>
                {m.goal}
              </span>
              <span
                className={`shrink-0 rounded-full px-1.5 py-0.5 text-2xs font-medium ring-1 ${STATUS_STYLE[m.status] ?? STATUS_STYLE.done}`}
              >
                {m.status}
              </span>
              <button
                onClick={() => void extract(m)}
                disabled={busy !== null}
                title="Run one headless Hermes turn over the review and write what it learned as memory notes"
                className="ml-auto inline-flex h-7 shrink-0 items-center gap-1.5 rounded-lg px-2 text-xs text-slate-300 ring-1 ring-line hover:bg-white/[0.04] disabled:opacity-50"
              >
                {busy === m.id ? (
                  <Loader2 className="size-3.5 animate-spin" />
                ) : (
                  <GraduationCap className="size-3.5" />
                )}
                Extract lessons
              </button>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
