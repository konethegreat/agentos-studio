import { Loader2, RefreshCw, Search, ShieldCheck } from "lucide-react";
import { useCallback, useEffect, useState } from "react";
import { useToast } from "../components/Toasts";
import { Badge, Button, Card } from "../components/ui";
import { api, errorText } from "../lib/api";
import { studio, type AuditEntry, type Policy, type PolicyView } from "../lib/studio";

const RISK: Record<string, string> = {
  high: "bg-red-500/10 text-red-300 ring-red-500/30",
  medium: "bg-amber-500/10 text-amber-300 ring-amber-500/30",
  low: "bg-white/5 text-slate-400 ring-white/10",
};

const POLICY_HINT: Record<Policy, string> = {
  allow: "Runs without asking. Still written to the audit log.",
  ask: "Shows an approval prompt every time.",
  deny: "Always refused, even if a chat or agent asks for it.",
};

export function SafetyCenter() {
  const toast = useToast();
  const [policies, setPolicies] = useState<PolicyView[]>([]);
  const [audit, setAudit] = useState<AuditEntry[]>([]);
  const [query, setQuery] = useState("");
  const [loading, setLoading] = useState(false);
  const [tools, setTools] = useState<string[]>([]);
  const [toolPolicy, setToolPolicy] = useState<Record<string, string>>({});

  const load = useCallback(async () => {
    setLoading(true);
    try {
      const [p, a] = await Promise.all([studio.safety.policies(), studio.safety.audit(400, query.trim() || null)]);
      setPolicies(p);
      setAudit(a);
    } catch (e) {
      toast("error", errorText(e));
    } finally {
      setLoading(false);
    }
  }, [query, toast]);

  useEffect(() => {
    void api.hubTools().then(setTools).catch(() => undefined);
    void api
      .toolPolicies()
      .then((rows) => setToolPolicy(Object.fromEntries(rows.map((r) => [r.tool, r.policy]))))
      .catch(() => undefined);
  }, []);

  async function setTool(tool: string, policy: "allow" | "ask" | "deny") {
    try {
      await api.setToolPolicy("agentos-hub", tool, policy);
      setToolPolicy((cur) => ({ ...cur, [tool]: policy }));
      toast("ok", `${tool}: ${policy}`);
    } catch (e) {
      toast("error", errorText(e));
    }
  }

  useEffect(() => {
    const t = window.setTimeout(() => void load(), 200);
    return () => window.clearTimeout(t);
  }, [load]);

  async function set(id: string, policy: Policy) {
    try {
      await studio.safety.setPolicy(id, policy);
      void load();
    } catch (e) {
      toast("error", errorText(e));
    }
  }

  const groups = [...new Set(policies.map((p) => p.group))];

  return (
    <div className="h-full overflow-y-auto">
      <div className="mx-auto max-w-5xl space-y-5 px-6 py-6">
        <div className="flex items-center gap-3">
          <ShieldCheck className="size-6 text-emerald-300" />
          <div className="flex-1">
            <h1 className="text-lg font-semibold text-slate-100">Safety</h1>
            <p className="text-xs text-slate-500">
              Permissions are checked in Studio's Rust core before anything runs, so they apply to buttons, chat commands and shortcuts alike. Secrets are never stored here: GitHub uses gh's keyring and provider keys stay in Hermes.
            </p>
          </div>
          <Button size="sm" variant="ghost" icon={loading ? <Loader2 className="size-3.5 animate-spin" /> : <RefreshCw className="size-3.5" />} onClick={() => void load()}>
            Reload
          </Button>
        </div>

        <Card className="p-0">
          <div className="border-b border-line px-4 py-3">
            <h2 className="text-sm font-semibold text-slate-100">Tool permissions (agentos-hub)</h2>
            <p className="mt-0.5 text-small text-slate-500">
              Enforced inside the MCP server, so an agent cannot call around it. Ask means refused in headless sessions, because there is nobody to prompt.
            </p>
          </div>
          {tools.length === 0 ? (
            <div className="px-4 py-3 text-xs text-slate-500">Reading the hub tool list...</div>
          ) : (
            tools.map((t) => (
              <div key={t} className="flex items-center gap-3 border-t border-line px-4 py-2">
                <span className="min-w-0 flex-1 font-mono text-small text-slate-300">{t}</span>
                <div className="flex rounded-lg bg-canvas p-0.5 ring-1 ring-line" role="radiogroup" aria-label={`Policy for ${t}`}>
                  {(["allow", "ask", "deny"] as const).map((v) => {
                    const active = (toolPolicy[t] ?? "allow") === v;
                    return (
                      <button
                        key={v}
                        role="radio"
                        aria-checked={active}
                        onClick={() => void setTool(t, v)}
                        className={`rounded-md px-2 py-1 text-label capitalize ${active ? "bg-white/10 text-slate-100" : "text-slate-500 hover:text-slate-300"}`}
                      >
                        {v}
                      </button>
                    );
                  })}
                </div>
              </div>
            ))
          )}
        </Card>

        <Card className="p-0">
          <div className="border-b border-line px-4 py-3">
            <h2 className="text-sm font-semibold text-slate-100">Permission policies</h2>
          </div>
          {groups.map((g) => (
            <div key={g}>
              <div className="bg-white/[0.02] px-4 py-1.5 text-caption font-semibold uppercase tracking-wider text-slate-500">{g}</div>
              {policies
                .filter((p) => p.group === g)
                .map((p) => (
                  <div key={p.id} className="flex items-center gap-3 border-t border-line px-4 py-2.5">
                    <div className="min-w-0 flex-1">
                      <div className="flex items-center gap-2 text-xs text-slate-200">
                        {p.label}
                        <Badge className={RISK[p.risk]}>{p.risk} risk</Badge>
                        {!p.reversible && <Badge className="bg-white/5 text-slate-400 ring-white/10">cannot be undone</Badge>}
                      </div>
                      <div className="font-mono text-caption text-slate-600">
                        {p.id} · default {p.default_policy}
                      </div>
                    </div>
                    <div className="flex rounded-lg bg-canvas p-0.5 ring-1 ring-line" role="radiogroup" aria-label={`Policy for ${p.label}`}>
                      {(["allow", "ask", "deny"] as Policy[]).map((v) => (
                        <button
                          key={v}
                          role="radio"
                          aria-checked={p.policy === v}
                          title={POLICY_HINT[v]}
                          onClick={() => void set(p.id, v)}
                          className={`h-7 rounded-md px-3 text-label font-medium capitalize ${
                            p.policy === v ? (v === "deny" ? "bg-red-500/20 text-red-200" : v === "ask" ? "bg-amber-500/15 text-amber-200" : "bg-emerald-500/15 text-emerald-200") : "text-slate-500 hover:text-slate-200"
                          }`}
                        >
                          {v}
                        </button>
                      ))}
                    </div>
                  </div>
                ))}
            </div>
          ))}
        </Card>

        <Card className="p-0">
          <div className="flex items-center gap-3 border-b border-line px-4 py-3">
            <h2 className="flex-1 text-sm font-semibold text-slate-100">Audit log</h2>
            <div className="flex w-64 items-center gap-2 rounded-lg bg-canvas px-2 ring-1 ring-line">
              <Search className="size-3.5 text-slate-500" />
              <input value={query} onChange={(e) => setQuery(e.target.value)} placeholder="Filter by action, target or detail" aria-label="Filter audit log" className="h-7 min-w-0 flex-1 bg-transparent text-xs text-slate-100 outline-none placeholder:text-slate-600" />
            </div>
          </div>
          {audit.length === 0 ? (
            <div className="px-4 py-8 text-center text-xs text-slate-600">Nothing recorded yet.</div>
          ) : (
            <table className="w-full text-left text-small">
              <thead className="text-caption uppercase tracking-wider text-slate-500">
                <tr>
                  <th className="px-4 py-2 font-medium">When</th>
                  <th className="px-2 py-2 font-medium">Action</th>
                  <th className="px-2 py-2 font-medium">Target</th>
                  <th className="px-2 py-2 font-medium">Result</th>
                </tr>
              </thead>
              <tbody>
                {audit.map((a) => (
                  <tr key={a.id} className="border-t border-line align-top" title={a.detail}>
                    <td className="whitespace-nowrap px-4 py-2 text-slate-500">{new Date(a.at * 1000).toLocaleString()}</td>
                    <td className="px-2 py-2 font-mono text-slate-300">{a.action}</td>
                    <td className="max-w-[320px] px-2 py-2 text-slate-400">
                      <div className="truncate">{a.target}</div>
                      {a.detail && <div className="truncate text-caption text-slate-600">{a.detail}</div>}
                    </td>
                    <td className={`px-2 py-2 ${a.outcome === "ok" ? "text-emerald-300" : a.outcome === "denied" ? "text-amber-300" : "text-red-300"}`}>{a.outcome}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
        </Card>
      </div>
    </div>
  );
}
