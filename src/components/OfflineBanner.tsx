import { useEffect, useState } from "react";
import { WifiOff } from "lucide-react";
import { api, errorText, events, type OfflineStatus } from "../lib/api";

/**
 * Offline banner: states a live condition, so it has no dismiss. The one control is automatic
 * local pinning, which is on unless the owner turns it off here.
 */
export function OfflineBanner() {
  const [st, setSt] = useState<OfflineStatus | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    void api.offlineStatus().then(setSt).catch(() => undefined);
    const off = events.offline(setSt);
    return () => {
      void off.then((f) => f());
    };
  }, []);

  if (!st || st.online) return null;

  const text = st.pin
    ? `Offline. Missions run on ${st.pin.local_model} locally; cloud lanes resume when the network returns.`
    : st.auto_local
      ? st.local_models.length > 0
        ? `Offline. ${st.local_models.length} local model(s) available; Studio pins one automatically.`
        : "Offline. Missions wait and resume when the network returns."
      : "Offline. Automatic local pinning is off, so missions wait and resume when the network returns.";

  async function flip(auto: boolean) {
    setBusy(true);
    setError(null);
    try {
      setSt(await api.offlineSetAutoLocal(auto));
    } catch (e) {
      setError(errorText(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div role="status" className="flex shrink-0 items-center gap-3 border-b border-amber-500/20 bg-amber-500/[0.07] px-5 py-2 text-xs">
      <WifiOff className="size-4 shrink-0 text-amber-300" />
      <div className="min-w-0 flex-1 text-slate-200" title={st.note}>
        {text}
        {error && <span className="text-red-300"> {error}</span>}
      </div>
      <button
        onClick={() => void flip(!st.auto_local)}
        disabled={busy}
        title="Automatic local pinning switches Hermes to a local model while offline and restores the previous provider when the network returns."
        className="rounded-md bg-amber-500/15 px-2.5 py-1 font-semibold text-amber-200 ring-1 ring-amber-500/30 hover:bg-amber-500/20 disabled:opacity-50"
      >
        {busy ? "Saving..." : st.auto_local ? "Stop automatic pinning" : "Turn on automatic pinning"}
      </button>
    </div>
  );
}
