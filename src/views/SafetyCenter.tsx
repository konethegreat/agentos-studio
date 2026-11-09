import { Loader2, RefreshCw, Search, ShieldCheck } from "lucide-react";
import { useCallback, useEffect, useState } from "react";
import { useToast } from "../components/Toasts";
import { Badge, Button, Card } from "../components/ui";
import { api, errorText, type ToolPolicyModel, type UnattendedProfile } from "../lib/api";
import { HermesAuditCard } from "../components/HermesAudit";
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

type ToolScope = { kind: "global" } | { kind: "agent" | "repo"; key: string };

function scopeValue(scope: ToolScope): string {
  return scope.kind === "global" ? "global" : `${scope.kind}:${scope.key}`;
}

function parseScope(value: string): ToolScope {
  if (value === "global") return { kind: "global" };
  const i = value.indexOf(":");
  const kind = value.slice(0, i) === "repo" ? "repo" : "agent";
  return { kind, key: value.slice(i + 1) };
}

function policyTone(p: string): string {
  return p === "deny" ? "bg-red-500/20 text-red-200" : p === "ask" ? "bg-amber-500/15 text-amber-200" : "bg-emerald-500/15 text-emerald-200";
}

const PROFILE_STATUS: Record<string, { label: string; tone: string }> = {
  enforced: { label: "Enforced", tone: "bg-emerald-500/15 text-emerald-200 ring-emerald-400/20" },
  ready: { label: "Ready for worker", tone: "bg-sky-500/15 text-sky-200 ring-sky-400/20" },
  hermes: { label: "Needs Hermes", tone: "bg-amber-500/15 text-amber-200 ring-amber-400/20" },
};

export function SafetyCenter() {
  const toast = useToast();
  const [policies, setPolicies] = useState<PolicyView[]>([]);
  const [audit, setAudit] = useState<AuditEntry[]>([]);
  const [query, setQuery] = useState("");
  const [loading, setLoading] = useState(false);
  const [model, setModel] = useState<ToolPolicyModel | null>(null);
  const [profile, setProfile] = useState<UnattendedProfile | null>(null);
  const [scope, setScope] = useState<ToolScope>({ kind: "global" });

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

  const loadModel = useCallback(async () => {
    try {
      const [m, p] = await Promise.all([api.toolPolicyModel(), api.unattendedProfile()]);
      setModel(m);
      setProfile(p);
    } catch (e) {
      toast("error", errorText(e));
    }
  }, [toast]);

  useEffect(() => {
    void loadModel();
  }, [loadModel]);

  const globalPolicy = (tool: string) => model?.global.find((g) => g.server === "agentos-hub" && g.tool === tool)?.policy ?? "allow";
  const scopedPolicy = (tool: string) =>
    scope.kind === "global"
      ? null
      : model?.scoped.find((s) => s.scope === scope.kind && s.key === scope.key && s.tool === tool && s.server === "agentos-hub")?.policy ?? null;
  const valueFor = (tool: string) => (scope.kind === "global" ? globalPolicy(tool) : scopedPolicy(tool));
  const effectiveFor = (tool: string) => scopedPolicy(tool) ?? globalPolicy(tool);

  async function setTool(tool: string, policy: "allow" | "ask" | "deny" | "inherit") {
    try {
      if (scope.kind === "global") {
        if (policy === "inherit") return;
        await api.setToolPolicy("agentos-hub", tool, policy);
      } else {
        await api.setScopedToolPolicy(scope.kind, scope.key, "agentos-hub", tool, policy === "inherit" ? null : policy);
      }
      await loadModel();
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

        <HermesAuditCard />

        <Card className="p-0">
          <div className="border-b border-line px-4 py-3">
            <div className="flex flex-wrap items-center gap-3">
              <div className="min-w-0 flex-1">
                <h2 className="text-sm font-semibold text-slate-100">Tool permissions (agentos-hub)</h2>
                <p className="mt-0.5 text-small text-slate-500">
                  Checked inside the MCP server, so an agent cannot call around it. A project override wins over an agent override, which wins over the global policy. Ask refuses in headless sessions, because there is nobody to prompt.
                </p>
              </div>
              <label className="flex items-center gap-2 text-small text-slate-400">
                Scope
                <select
                  aria-label="Permission scope"
                  value={scopeValue(scope)}
                  onChange={(e) => setScope(parseScope(e.target.value))}
                  className="h-8 rounded-lg bg-canvas px-2 text-small text-slate-100 ring-1 ring-line outline-none"
                >
                  <option value="global">Global</option>
                  <optgroup label="Agent">
                    {(model?.agents ?? []).map((a) => (
                      <option key={a} value={`agent:${a}`}>
                        {a}
                      </option>
                    ))}
                  </optgroup>
                  <optgroup label="Project">
                    {(model?.repos ?? []).map((r) => (
                      <option key={r} value={`repo:${r}`}>
                        {r}
                      </option>
                    ))}
                  </optgroup>
                </select>
              </label>
            </div>
            <p className="mt-2 text-caption text-slate-600">
              {scope.kind === "global" && "This is the default for every agent and project that has no override."}
              {scope.kind === "agent" && `Applies to ${scope.key} wherever it calls the hub. A project override still wins.`}
              {scope.kind === "repo" && `Applies to hub sessions that declare this project through AGENTOS_REPO. A session without it follows only the agent and global policies.`}
            </p>
          </div>
          {!model ? (
            <div className="px-4 py-3 text-xs text-slate-500">Reading the hub tool list...</div>
          ) : model.tools.length === 0 ? (
            <div className="px-4 py-3 text-xs text-slate-500">The hub reports no tools.</div>
          ) : (
            model.tools.map((t) => {
              const own = valueFor(t);
              const inherited = scope.kind !== "global" && own === null;
              return (
                <div key={t} className="flex flex-wrap items-center gap-x-3 gap-y-2 border-t border-line px-4 py-2">
                  <div className="min-w-0 flex-1">
                    <span className="font-mono text-small text-slate-300">{t}</span>
                    {scope.kind !== "global" && (
                      <span className="ml-2 text-caption text-slate-600">
                        {inherited ? `inherits global ${globalPolicy(t)}` : `set to ${own} here`}
                        {scope.kind === "repo" && !inherited && ` · agents without this override follow their own policy`}
                        {scope.kind === "agent" && ` · effective ${effectiveFor(t)}`}
                      </span>
                    )}
                  </div>
                  <div className="flex rounded-lg bg-canvas p-0.5 ring-1 ring-line" role="radiogroup" aria-label={`Policy for ${t} in ${scopeValue(scope)}`}>
                    {(scope.kind === "global" ? (["allow", "ask", "deny"] as const) : (["inherit", "allow", "ask", "deny"] as const)).map((v) => {
                      const active = v === "inherit" ? inherited : !inherited && own === v;
                      return (
                        <button
                          key={v}
                          role="radio"
                          aria-checked={active}
                          title={v === "inherit" ? "Use the next wider scope" : POLICY_HINT[v]}
                          onClick={() => void setTool(t, v)}
                          className={`rounded-md px-2 py-1 text-label capitalize ${active ? (v === "inherit" ? "bg-white/10 text-slate-200" : policyTone(v)) : "text-slate-500 hover:text-slate-300"}`}
                        >
                          {v}
                        </button>
                      );
                    })}
                  </div>
                </div>
              );
            })
          )}
        </Card>

        <Card className="p-0">
          <div className="border-b border-line px-4 py-3">
            <h2 className="text-sm font-semibold text-slate-100">Unattended runs</h2>
            <p className="mt-0.5 text-small text-slate-500">
              What a mission worker gets when nobody is watching. Enforced is live now, Ready for worker takes effect when the worker starts through this environment, Needs Hermes depends on the runtime or its egress support.
            </p>
          </div>
          {!profile ? (
            <div className="px-4 py-3 text-xs text-slate-500">Reading the profile...</div>
          ) : (
            <>
              {profile.guarantees.map((g) => {
                const s = PROFILE_STATUS[g.status] ?? PROFILE_STATUS.hermes;
                return (
                  <div key={g.id} className="border-t border-line px-4 py-2.5">
                    <div className="flex flex-wrap items-center gap-2">
                      <span className="text-xs text-slate-200">{g.title}</span>
                      <Badge className={s.tone}>{s.label}</Badge>
                    </div>
                    <p className="mt-0.5 text-small text-slate-500">{g.detail}</p>
                  </div>
                );
              })}
              <div className="border-t border-line px-4 py-2.5">
                <div className="text-caption font-semibold uppercase tracking-wider text-slate-500">Environment kept</div>
                <div className="mt-1.5 flex flex-wrap gap-1.5">
                  {profile.env_allowlist.map((v) => (
                    <code key={v} className="rounded bg-canvas px-1.5 py-0.5 text-caption text-slate-400 ring-1 ring-line">
                      {v}
                    </code>
                  ))}
                  <code className="rounded bg-canvas px-1.5 py-0.5 text-caption text-slate-400 ring-1 ring-line">AGENTOS_REPO (when known)</code>
                  <code className="rounded bg-canvas px-1.5 py-0.5 text-caption text-slate-400 ring-1 ring-line">AGENTOS_UNATTENDED</code>
                  <code className="rounded bg-canvas px-1.5 py-0.5 text-caption text-slate-400 ring-1 ring-line">AGENTOS_VAULT</code>
                </div>
                <p className="mt-1.5 text-caption text-slate-600">
                  Everything else, API keys and tokens included, is dropped. The worker switches to <code className="text-slate-400">permissions::unattended_env</code> through a REQUEST on the coordination note.
                </p>
              </div>
            </>
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
