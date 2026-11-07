import { useEffect, useState } from "react";
import { CheckCircle2, Circle, ExternalLink, KeyRound, RefreshCw } from "lucide-react";
import { api, errorText } from "../lib/api";
import { studio } from "../lib/studio";
import { useToast } from "./Toasts";
import { Button, Card, inputClass } from "./ui";

type Connection = {
  id: string;
  label: string;
  connected: boolean;
  detail: string;
  needs: string;
  env_var: string | null;
  setup: string | null;
  setup_label: string | null;
  link: string | null;
};

/**
 * Connect the subscriptions an agent can actually use. A key is written straight to the Hermes
 * env file, which is where the runtime reads it, and never stored in Studio or shown again.
 */
export function ConnectCard() {
  const toast = useToast();
  const [rows, setRows] = useState<Connection[] | null>(null);
  const [key, setKey] = useState("");
  const [busy, setBusy] = useState(false);

  async function load() {
    try {
      setRows(await api.providerConnections());
    } catch (e) {
      toast("error", errorText(e));
    }
  }

  useEffect(() => {
    void load();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  async function connect(c: Connection) {
    if (!c.setup) return;
    try {
      await api.repair(c.setup);
      toast("info", `${c.setup_label ?? "Setup"} started. Follow the window that opens; progress is in the Setup console.`);
    } catch (e) {
      toast("error", errorText(e));
    }
  }

  async function save() {
    setBusy(true);
    try {
      toast("ok", await api.saveProviderKey("openrouter", key));
      setKey("");
      await load();
    } catch (e) {
      toast("error", errorText(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <Card className="p-0">
      <div className="flex items-center gap-2 border-b border-line px-4 py-3">
        <KeyRound className="size-4 text-emerald-300" />
        <div className="flex-1">
          <h2 className="text-sm font-semibold text-slate-100">Connect your subscriptions</h2>
          <p className="text-small text-slate-500">
            A subscription is billed flat, so it carries the volume. An API key is metered, and only
            the key name is ever shown back.
          </p>
        </div>
        <Button size="sm" variant="ghost" icon={<RefreshCw className="size-3.5" />} onClick={() => void load()}>
          Check
        </Button>
      </div>

      {rows === null ? (
        <div className="px-4 py-3 text-xs text-slate-500">Checking what is connected...</div>
      ) : (
        rows.map((c) => (
          <div key={c.id} className="border-t border-line px-4 py-3">
            <div className="flex items-center gap-2">
              {c.connected ? (
                <CheckCircle2 className="size-4 text-ok" />
              ) : (
                <Circle className="size-4 text-slate-600" />
              )}
              <span className="text-body font-medium text-slate-200">{c.label}</span>
              <span className={`rounded-full px-1.5 py-0.5 text-2xs font-medium ring-1 ${c.connected ? "bg-emerald-500/10 text-emerald-300 ring-emerald-500/25" : "bg-white/5 text-slate-400 ring-white/10"}`}>
                {c.connected ? "connected" : "not connected"}
              </span>
            </div>
            <p className="mt-1 text-small text-slate-500">{c.connected ? c.detail : c.needs}</p>
            {!c.connected && (c.setup || c.link) && (
              <div className="mt-2 flex flex-wrap items-center gap-3">
                {c.setup && (
                  <Button size="sm" onClick={() => void connect(c)}>
                    {c.setup_label ?? "Connect"}
                  </Button>
                )}
                {c.link && (
                  <button
                    onClick={() => void studio.os.openExternal(c.link!).catch((e: unknown) => toast("error", errorText(e)))}
                    className="inline-flex items-center gap-1 text-small text-slate-400 hover:text-slate-200"
                  >
                    <ExternalLink className="size-3" aria-hidden /> {c.env_var ? "Get a key" : "Learn more"}
                  </button>
                )}
              </div>
            )}
            {c.env_var && (
              <div className="mt-2 flex gap-2">
                <input
                  type="password"
                  value={key}
                  onChange={(e) => setKey(e.target.value)}
                  placeholder={c.env_var}
                  aria-label={`${c.label} API key`}
                  className={`${inputClass} h-8 flex-1 font-mono text-label`}
                />
                <Button size="sm" onClick={() => void save()} disabled={busy || !key.trim()}>
                  Save key
                </Button>
              </div>
            )}
          </div>
        ))
      )}
    </Card>
  );
}
