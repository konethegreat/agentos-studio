import { useEffect, useRef, useState } from "react";
import { AlertTriangle, Gauge } from "lucide-react";
import { api } from "../lib/api";

type Usage = {
  model: string | null;
  input_tokens: number;
  output_tokens: number;
  cost_usd: number;
  messages: number;
  compacted_messages: number;
  context_limit: number | null;
  context_used_pct: number | null;
  context_estimated: boolean;
};

type Credits = {
  provider: string;
  kind: string;
  remaining_usd: number | null;
  used_usd: number | null;
  note: string;
};

const fmt = (n: number) => (n >= 1000 ? `${(n / 1000).toFixed(1)}k` : `${n}`);

/**
 * How full the context window is, what the session has cost, and whether Hermes has already had to
 * compact. Compaction is where answer quality drops, so it is shown before it happens, not after.
 */
export function UsageBar({
  sessionId,
  refresh = 0,
  onUsage,
}: {
  sessionId: string | null;
  /** Bump after a turn settles so the figures follow the conversation. */
  refresh?: number;
  /** Context fill and compaction count, for the chat guides. */
  onUsage?: (u: { contextPct: number | null; compacted: number } | null) => void;
}) {
  const [usage, setUsage] = useState<Usage | null>(null);
  const [credits, setCredits] = useState<Credits | null>(null);

  useEffect(() => {
    if (!sessionId) {
      setUsage(null);
      return;
    }
    let live = true;
    void api
      .sessionUsage(sessionId)
      .then((u) => live && setUsage(u as Usage | null))
      .catch(() => undefined);
    return () => {
      live = false;
    };
  }, [sessionId, refresh]);

  const report = useRef(onUsage);
  report.current = onUsage;
  useEffect(() => {
    report.current?.(usage ? { contextPct: usage.context_limit ? usage.context_used_pct : null, compacted: usage.compacted_messages } : null);
  }, [usage]);

  useEffect(() => {
    void api
      .providerCredits()
      .then((all) => setCredits(all.find((c) => c.provider === "openrouter") ?? null))
      .catch(() => undefined);
  }, []);

  if (!usage) return null;
  const pct = usage.context_used_pct ?? 0;
  const tight = pct >= 80;
  const warn = pct >= 65 && pct < 80;

  return (
    <div className="flex flex-wrap items-center gap-x-4 gap-y-1 border-b border-line/60 bg-white/[0.015] px-4 py-1.5 text-label text-slate-500">
      <span className="inline-flex items-center gap-1.5">
        <Gauge className="size-3.5" />
        <span className="text-slate-400">{usage.model ?? "no model"}</span>
      </span>
      {usage.context_limit ? (
        <span className="inline-flex items-center gap-2" title={`${usage.context_estimated ? "Estimated from text length: " : ""}${usage.context_used_pct?.toFixed(1)}% of ${usage.context_limit.toLocaleString()} tokens`}>
          <span className="relative h-1.5 w-28 overflow-hidden rounded-full bg-white/10">
            <span
              className={`absolute inset-y-0 left-0 rounded-full ${tight ? "bg-red-400" : warn ? "bg-amber-400" : "bg-emerald-400"}`}
              style={{ width: `${Math.min(100, pct)}%` }}
            />
          </span>
          {usage.context_estimated ? "~" : ""}
          {pct.toFixed(0)}% of {fmt(usage.context_limit)}
        </span>
      ) : (
        <span title="This model has no context limit in the local models cache">context limit unknown</span>
      )}
      <span>
        {fmt(usage.input_tokens)} in / {fmt(usage.output_tokens)} out
      </span>
      <span title="Reported by Hermes for this session">${usage.cost_usd.toFixed(4)}</span>
      <span>{usage.messages} msgs</span>
      {usage.compacted_messages > 0 && (
        <span className="inline-flex items-center gap-1 text-amber-300" title="Hermes has compressed this session; detail may be lossy">
          <AlertTriangle className="size-3" /> {usage.compacted_messages} compacted
        </span>
      )}
      {credits && (
        <span
          className="ml-auto inline-flex items-center gap-1"
          title={credits.note}
        >
          {credits.remaining_usd !== null ? (
            <>
              OpenRouter ${credits.remaining_usd.toFixed(2)} left
              {credits.used_usd !== null ? ` · $${credits.used_usd.toFixed(2)} used` : ""}
            </>
          ) : (
            credits.note
          )}
        </span>
      )}
    </div>
  );
}
