import { useEffect, useState } from "react";
import { CheckCircle2, Circle, CircleAlert, ExternalLink, KeyRound, RefreshCw } from "lucide-react";
import { api, errorText, type ProviderHealthReport } from "../lib/api";
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

const STATE_TEXT: Record<string, string> = {
  ok: "working",
  signed_out: "signed out",
  plugin_disabled: "plugin off",
  rate_limited: "usage limit",
  failing: "failing",
  not_set_up: "not set up",
};

/**
 * Whether each subscription Hermes runs on can run right now, from Hermes' own sign-in state and
 * the failures it logged, with the one command that fixes each problem. Checked every 5 minutes.
 */
function ConnectionHealth() {
  const toast = useToast();
  const [r, setR] = useState<ProviderHealthReport | null>(null);
  const [busy, setBusy] = useState(false);
  async function load(fresh: boolean) {
    setBusy(true);
    try {
      setR(await api.providerHealth(fresh));
    } catch (e) {
      toast("error", errorText(e));
    } finally {
      setBusy(false);
    }
  }
  useEffect(() => {
    void load(false);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);
  async function fix(id: string, action: string, label: string) {
    try {
      await api.providerFix(id, action);
      toast("info", `${label}: finish it in the window that opened, then press Check now.`);
    } catch (e) {
      toast("error", errorText(e));
    }
  }
  return (
    <div className="border-t border-line px-4 py-3">
      <div className="mb-2 flex items-center justify-between">
        <div className="text-small font-semibold text-slate-200">Can Hermes run right now?</div>
        <Button size="sm" variant="ghost" busy={busy} icon={<RefreshCw className="size-3.5" />} onClick={() => void load(true)}>
          Check now
        </Button>
      </div>
      {r?.advice && (
        <div className="mb-2 rounded-lg bg-amber-500/[0.06] px-3 py-2 text-small text-amber-200 ring-1 ring-amber-500/25">
          {r.advice}
          <button className="ml-2 underline" onClick={() => void fix("openrouter", "add_fallback", "Add a fallback")}>
            Add a fallback
          </button>
        </div>
      )}
      {r === null ? (
        <div className="text-small text-slate-500">Asking Hermes...</div>
      ) : (
        <ul className="space-y-1.5">
          {r.providers.map((h) => (
            <li key={h.id} className="flex items-start gap-2">
              {h.state === "ok" ? <CheckCircle2 className="mt-0.5 size-4 shrink-0 text-ok" /> : h.state === "not_set_up" ? <Circle className="mt-0.5 size-4 shrink-0 text-slate-600" /> : <CircleAlert className="mt-0.5 size-4 shrink-0 text-warn" />}
              <div className="min-w-0 flex-1">
                <div className="text-small text-slate-200">
                  {h.label}
                  {h.in_use && <span className="ml-1.5 text-2xs text-emerald-300">{r.active === h.id ? "active" : "fallback"}</span>}
                  <span className="ml-1.5 text-2xs text-muted">{STATE_TEXT[h.state] ?? h.state}</span>
                </div>
                <div className="text-label text-muted">{h.detail}</div>
              </div>
              {h.fix && (
                <Button size="sm" onClick={() => void fix(h.id, h.fix![0], h.fix![1])}>
                  {h.fix[1]}
                </Button>
              )}
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

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
      <ConnectionHealth />
    </Card>
  );
}
