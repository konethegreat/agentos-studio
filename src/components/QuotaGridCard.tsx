import { useCallback, useEffect, useState } from "react";
import { Activity, Route as RouteIcon } from "lucide-react";
import { api, errorText, type CanaryLane, type QuotaForecast, type QuotaLane } from "../lib/api";
import { Badge, Card } from "./ui";

/**
 * The quota grid (docs/BEYOND.md, B2): Hermes' own routes in order, the primary then the
 * fallback chain, each open or closed and why. A new Hermes mission runs on the first open one.
 */
export function QuotaGridCard({ tick }: { tick: number }) {
  const [lanes, setLanes] = useState<QuotaLane[] | null>(null);
  const [error, setError] = useState("");

  const load = useCallback(async (refresh: boolean) => {
    try {
      setLanes(await api.quotaLanes(refresh));
      setError("");
    } catch (e) {
      setError(errorText(e));
    }
  }, []);

  useEffect(() => {
    void load(tick > 0);
  }, [load, tick]);

  const [forecast, setForecast] = useState<QuotaForecast[]>([]);
  const [login, setLogin] = useState<boolean | null>(null);

  useEffect(() => {
    void api.quotaForecast(false).then(setForecast).catch(() => setForecast([]));
    void api.autostartGet().then(setLogin).catch(() => setLogin(null));
  }, [tick, lanes]);

  const toggleLogin = async () => {
    try {
      setLogin(await api.autostartSet(!login));
    } catch (e) {
      setError(errorText(e));
    }
  };

  const [canary, setCanary] = useState<CanaryLane[]>([]);
  const [probing, setProbing] = useState(false);

  useEffect(() => {
    void api.canaryStatus().then(setCanary).catch(() => setCanary([]));
  }, [tick, lanes]);

  const probeNow = async () => {
    setProbing(true);
    try {
      setCanary(await api.canaryRunNow());
      await load(false);
    } catch (e) {
      setError(errorText(e));
    } finally {
      setProbing(false);
    }
  };

  const canaryFor = (l: QuotaLane) =>
    canary.find((c) => c.provider.toLowerCase() === l.provider.toLowerCase() && c.model.toLowerCase() === l.model.toLowerCase());

  const first = lanes?.findIndex((l) => l.open) ?? -1;
  const heavy = forecast.find((f) => f.heavy_ready);
  const primaryCast = forecast[0];

  return (
    <Card className="space-y-2 p-4">
      <div className="flex flex-wrap items-baseline gap-2">
        <RouteIcon className="size-4 self-center text-sky-300" />
        <h2 className="text-sm font-semibold text-slate-100">Quota grid</h2>
        <span className="text-label text-slate-500">
          Hermes' primary model, then its fallback chain. A new Hermes mission starts on the first open route; when none
          is open it waits for the soonest reset instead of failing.
        </span>
      </div>
      {error && <div className="text-label text-rose-300">{error}</div>}
      {primaryCast && (
        <p className="text-label text-slate-400">
          {heavy
            ? "Heavy missions can start now: the primary route is open."
            : `Heavy missions wait for the primary route (${primaryCast.model}): ${primaryCast.summary}.`}
        </p>
      )}
      {login !== null && (
        <label className="flex items-center gap-2 text-label text-slate-400">
          <input type="checkbox" checked={login} onChange={() => void toggleLogin()} />
          Start AgentOS Studio at Windows login (minimized to the tray) so the fleet resumes after a reboot.
        </label>
      )}
      {lanes === null ? (
        <p className="text-small text-slate-500">Reading Hermes' routes...</p>
      ) : lanes.length === 0 ? (
        <p className="text-small text-slate-500">Hermes reports no model routes.</p>
      ) : (
        <ol className="space-y-1.5">
          {lanes.map((l, i) => (
            <li key={`${l.provider}/${l.model}`} className="flex flex-wrap items-center gap-2 text-small">
              <span className="w-5 text-right font-mono text-label text-slate-500">{i + 1}</span>
              <span className="font-mono text-slate-200">{l.model}</span>
              <span className="text-label text-slate-500">{l.provider}</span>
              {l.primary && <Badge className="bg-slate-500/10 text-slate-300 ring-slate-500/25">primary</Badge>}
              {l.open ? (
                <Badge className="bg-emerald-500/10 text-emerald-300 ring-emerald-500/25">{i === first ? "next work runs here" : "open"}</Badge>
              ) : (
                <Badge className="bg-amber-500/10 text-amber-300 ring-amber-500/25">closed</Badge>
              )}
              {l.headroom_pct !== null && (
                <span className="flex items-center gap-1.5" title={`${l.headroom_pct.toFixed(0)}% left in the tightest window`}>
                  <span className="h-1.5 w-20 overflow-hidden rounded-full bg-slate-700/60" aria-hidden="true">
                    <span
                      className={`block h-full ${l.headroom_pct < 10 ? "bg-amber-400" : "bg-emerald-400"}`}
                      style={{ width: `${Math.max(2, Math.min(100, l.headroom_pct))}%` }}
                    />
                  </span>
                  <span className="text-label text-slate-400">{l.headroom_pct.toFixed(0)}%</span>
                </span>
              )}
              <span className="min-w-0 flex-1 text-label text-slate-500">{l.why}</span>
              <CanaryNote state={canaryFor(l)} />
            </li>
          ))}
        </ol>
      )}
      <div className="flex flex-wrap items-center gap-2 border-t border-line pt-2">
        <Activity className="size-3.5 text-sky-300" />
        <span className="min-w-0 flex-1 text-label text-slate-500">
          Canary: every 30 minutes each flat-rate route gets one tiny question with a known answer. Two failures in a
          row close the route here until a probe passes again. Metered routes are not probed.
        </span>
        <button
          type="button"
          onClick={() => void probeNow()}
          disabled={probing}
          className="h-7 rounded-lg bg-white/[0.06] px-2.5 text-label font-medium text-slate-200 hover:bg-white/[0.1] disabled:opacity-50"
        >
          {probing ? "Probing (up to a minute per route)..." : "Probe now"}
        </button>
      </div>
    </Card>
  );
}

const CANARY_LABEL: Record<string, string> = {
  ok: "canary ok",
  slow: "canary slow",
  wrong_answer: "wrong answer",
  malformed_tool_call: "malformed tool call",
  no_reply: "no reply",
  auth: "sign-in problem",
  quota: "out of quota",
  fell_back: "answered by another route",
};

function ago(at: number): string {
  const s = Math.max(0, Math.floor(Date.now() / 1000) - at);
  if (s < 90) return `${s}s ago`;
  if (s < 5400) return `${Math.round(s / 60)}m ago`;
  return `${Math.round(s / 3600)}h ago`;
}

function CanaryNote({ state }: { state: CanaryLane | undefined }) {
  if (!state) return null;
  if (state.skipped) return <span className="w-full pl-7 text-label text-slate-600">Canary: metered route, not probed.</span>;
  if (!state.outcome) return <span className="w-full pl-7 text-label text-slate-600">Canary: not probed yet.</span>;
  const good = state.outcome === "ok" || state.outcome === "slow";
  return (
    <span className="flex w-full items-center gap-2 pl-7 text-label" title={state.detail}>
      <Badge
        className={
          good
            ? "bg-emerald-500/10 text-emerald-300 ring-emerald-500/25"
            : state.closed
              ? "bg-rose-500/10 text-rose-300 ring-rose-500/25"
              : "bg-amber-500/10 text-amber-300 ring-amber-500/25"
        }
      >
        {CANARY_LABEL[state.outcome] ?? state.outcome}
      </Badge>
      <span className="min-w-0 flex-1 truncate text-slate-500">
        {state.detail} ({ago(state.at)}
        {state.fails > 0 ? `, ${state.fails} in a row` : ""}
        {state.closed ? ", route closed" : ""})
      </span>
    </span>
  );
}
