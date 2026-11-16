import { useCallback, useEffect, useState } from "react";
import { Route as RouteIcon } from "lucide-react";
import { api, errorText, type QuotaForecast, type QuotaLane } from "../lib/api";
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
            </li>
          ))}
        </ol>
      )}
    </Card>
  );
}
