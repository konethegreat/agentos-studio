import { useEffect, useState } from "react";
import { Bot, CheckCircle2, CircleAlert, Gauge, Pencil, RefreshCw, Terminal } from "lucide-react";
import { api, errorText, type AgentCard, type InstalledAgent } from "../lib/api";
import { Badge, Button, Card, Empty, Modal, inputClass } from "../components/ui";
import { useToast } from "../components/Toasts";

const HEALTH_STYLE: Record<string, string> = {
  ready: "bg-emerald-500/10 text-emerald-300 ring-emerald-500/25",
  "ide only": "bg-sky-500/10 text-sky-300 ring-sky-500/25",
  "low quota": "bg-amber-500/10 text-amber-300 ring-amber-500/25",
  "read-only on the hub": "bg-orange-500/10 text-orange-300 ring-orange-500/25",
  "not installed": "bg-slate-500/10 text-slate-400 ring-slate-500/25",
};

const COST_LABEL: Record<string, string> = {
  flat: "Flat subscription",
  metered: "Metered",
  local: "Local",
};

/**
 * Fleet > Agents: the agent registry. Every card here is what the `agents` hub tool returns, so
 * an agent choosing who to hand work to sees exactly what the owner sees.
 */
export function Agents() {
  const toast = useToast();
  const [cards, setCards] = useState<AgentCard[] | null>(null);
  const [busy, setBusy] = useState(false);
  const [editing, setEditing] = useState<AgentCard | null>(null);
  const [draft, setDraft] = useState("");

  async function load(refresh = false) {
    setBusy(true);
    try {
      setCards(await api.agentCards(refresh));
    } catch (e) {
      toast("error", errorText(e));
    } finally {
      setBusy(false);
    }
  }

  useEffect(() => {
    void load();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  async function saveStrengths() {
    if (!editing) return;
    try {
      const list = draft.split(",").map((s) => s.trim()).filter(Boolean);
      await api.setAgentStrengths(editing.name, list);
      toast("ok", list.length ? `Saved strengths for ${editing.label}` : `${editing.label} is back on its default strengths`);
      setEditing(null);
      await load();
    } catch (e) {
      toast("error", errorText(e));
    }
  }

  return (
    <div className="flex h-full flex-col gap-4 overflow-y-auto p-6">
      <div className="flex items-start justify-between gap-4">
        <div>
          <h1 className="flex items-center gap-2 text-lg font-semibold text-slate-100">
            <Bot className="size-5 text-indigo-300" /> Agents
          </h1>
          <p className="mt-1 max-w-3xl text-sm text-slate-400">
            Who can take work, how each one starts, what it costs and how much quota it has left. Agents read the same
            registry through the <code className="text-slate-300">agents</code> hub tool before they hand work or a question
            to each other.
          </p>
        </div>
        <Button onClick={() => void load(true)} disabled={busy}>
          <RefreshCw className={`size-4 ${busy ? "animate-spin" : ""}`} /> Read quota again
        </Button>
      </div>

      {cards === null ? (
        <p className="text-sm text-slate-500">Reading the registry...</p>
      ) : cards.length === 0 ? (
        <Empty icon={<Bot className="size-6" />} title="No agents" hint="Install Hermes or Claude Code in Setup." />
      ) : (
        <div className="grid gap-4 lg:grid-cols-2">
          {cards.map((c) => (
            <Card key={c.name} className="flex flex-col gap-3 p-4">
              <div className="flex items-center justify-between gap-2">
                <div className="flex items-center gap-2">
                  {c.health === "ready" ? (
                    <CheckCircle2 className="size-4 text-emerald-400" />
                  ) : (
                    <CircleAlert className="size-4 text-amber-400" />
                  )}
                  <span className="font-medium text-slate-100">{c.label}</span>
                  <span className="text-xs text-slate-500">{c.name}</span>
                </div>
                <Badge className={HEALTH_STYLE[c.health] ?? HEALTH_STYLE["not installed"]}>{c.health}</Badge>
              </div>
              <p className="text-xs text-slate-400">{c.health_detail}</p>

              <div className="flex items-start gap-2 text-xs text-slate-300">
                <Terminal className="mt-0.5 size-3.5 shrink-0 text-slate-500" />
                <code className="break-all">{c.start}</code>
              </div>

              <div className="grid grid-cols-2 gap-3 text-xs">
                <div>
                  <div className="text-slate-500">Cost model</div>
                  <div className="text-slate-200">{COST_LABEL[c.cost_model] ?? c.cost_model}</div>
                  {c.provider && <div className="truncate text-slate-500">{c.provider}</div>}
                </div>
                <div>
                  <div className="flex items-center gap-1 text-slate-500">
                    <Gauge className="size-3.5" /> Quota
                  </div>
                  <div className="text-slate-200">{c.quota.summary}</div>
                  {c.quota.headroom_pct !== null && (
                    <div className="mt-1 h-1.5 w-full overflow-hidden rounded bg-slate-800" aria-hidden>
                      <div
                        className={`h-full ${c.quota.headroom_pct < 10 ? "bg-amber-400" : "bg-emerald-400"}`}
                        style={{ width: `${Math.max(2, Math.min(100, c.quota.headroom_pct))}%` }}
                      />
                    </div>
                  )}
                </div>
              </div>

              <div className="text-xs">
                <div className="flex items-center justify-between text-slate-500">
                  <span>Strengths ({c.strengths_source === "settings" ? "your settings" : "defaults"}, then measured)</span>
                  <button
                    className="flex items-center gap-1 text-slate-400 hover:text-slate-200"
                    aria-label={`Edit strengths for ${c.label}`}
                    onClick={() => {
                      setEditing(c);
                      setDraft(c.strengths.filter((s) => !s.endsWith("(measured)")).join(", "));
                    }}
                  >
                    <Pencil className="size-3" /> Edit
                  </button>
                </div>
                <div className="mt-1 flex flex-wrap gap-1">
                  {c.strengths.map((s) => (
                    <Badge key={s} className="bg-indigo-500/10 text-indigo-200 ring-indigo-500/25">
                      {s}
                    </Badge>
                  ))}
                </div>
              </div>

              <div className="flex flex-wrap items-center gap-3 text-xs text-slate-400">
                <span>
                  Missions: {c.measured.approvals} approved of {c.measured.attempts} decided
                  {c.measured.attempts > 0 && <> (score {c.measured.score.toFixed(0)})</>}
                </span>
                <span>Tools: {c.tools.join(", ")}</span>
                <span>Hub identity: {c.hub_identity ? "yes" : "no"}</span>
              </div>
            </Card>
          ))}
        </div>
      )}

      <AgentPacks />

      {editing && (
        <Modal
          title={`Strengths for ${editing.label}`}
          onClose={() => setEditing(null)}
          footer={
            <>
              <Button onClick={() => setEditing(null)}>Cancel</Button>
              <Button variant="primary" onClick={() => void saveStrengths()}>
                Save
              </Button>
            </>
          }
        >
          <p className="mb-2 text-xs text-slate-400">
            Comma separated. Leave empty to go back to the defaults. Measured strengths are added from mission outcomes.
          </p>
          <input className={inputClass} value={draft} onChange={(e) => setDraft(e.target.value)} autoFocus />
        </Modal>
      )}
    </div>
  );
}

/**
 * Installed agent packs: tailored agents with a persona, a model policy and a ceiling the host
 * enforces. Shown per agent the pack supports, with what it gets and what was dropped and why.
 */
function AgentPacks() {
  const [packs, setPacks] = useState<InstalledAgent[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    api
      .agentPacksInstalled()
      .then(setPacks)
      .catch((e) => setError(errorText(e)));
  }, []);
  return (
    <div className="mt-6">
      <div className="mb-2 flex items-baseline gap-2">
        <h3 className="text-sm font-semibold text-slate-100">Agent packs</h3>
        <span className="text-label text-slate-500">Installed from Brain, Capabilities. Choose one under Agent pack when you create a mission.</span>
      </div>
      {error && <div className="text-label text-rose-300">{error}</div>}
      {packs && packs.length === 0 && <Empty icon={<Bot className="size-5" />} title="No agent packs installed" hint="Install one from examples/capabilities/agents through Brain, Capabilities, Capability packs." />}
      <div className="grid gap-3 lg:grid-cols-2">
        {packs?.map((p) => (
          <Card key={p.entry.id} className="space-y-2 p-4">
            <div className="flex flex-wrap items-center gap-1.5">
              <span className="text-sm font-semibold text-slate-100">{p.entry.name}</span>
              <Badge className="bg-white/5 text-slate-400 ring-white/10">v{p.entry.version}</Badge>
              <Badge className={p.entry.tier === "official" ? "bg-emerald-500/10 text-emerald-300 ring-emerald-500/25" : p.entry.tier === "verified" ? "bg-sky-500/10 text-sky-300 ring-sky-500/25" : "bg-amber-500/10 text-amber-300 ring-amber-500/25"}>{p.entry.tier}</Badge>
              <span className="text-label text-slate-500">by {p.entry.publisher}</span>
            </div>
            {p.status !== "ok" && <div className="text-label text-rose-300">{p.status}</div>}
            <div className="text-label text-slate-400">
              Mode {p.spec.permission_mode} . model policy {p.spec.model_policy.join(" then ")} . {p.spec.unattended ? "may run unattended" : "supervised only"}
            </div>
            {p.ceilings.map((c) => (
              <div key={c.agent} className="rounded-md bg-canvas/60 px-2.5 py-2 text-label">
                <div className="flex items-center gap-1.5">
                  <span className="font-medium text-slate-300">{c.agent}</span>
                  <span className={c.unattended_ok ? "text-emerald-300" : "text-slate-500"}>{c.unattended_ok ? "unattended allowed" : "supervised"}</span>
                </div>
                <div className="text-slate-400">Gets: {c.tools.length ? c.tools.join(", ") : "nothing"}</div>
                {c.agent === "claude-code" && <div className="truncate text-slate-500" title={c.claude_allowed.join(" ")}>Allowlist: {c.claude_allowed.join(", ") || "none"} . mode {c.claude_mode}</div>}
                {c.agent === "hermes" && <div className="text-slate-500">Toolsets: {c.hermes_toolsets.join(", ") || "none"}</div>}
                {c.dropped.map((d) => (
                  <div key={d.tool} className="text-amber-300/80">
                    Dropped {d.tool}: {d.reason}
                  </div>
                ))}
              </div>
            ))}
          </Card>
        ))}
      </div>
    </div>
  );
}
