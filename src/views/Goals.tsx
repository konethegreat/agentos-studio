import { useEffect, useMemo, useState } from "react";
import { CheckCircle2, ChevronDown, ChevronRight, Flag, Plus, RefreshCw, Target, Trash2, X } from "lucide-react";
import { api, errorText, type AutonomyLevel, type Escalation, type GoalCheck, type GoalDetail, type NewGoal, type Project } from "../lib/api";
import { Badge, Button, Card, Empty, inputClass } from "../components/ui";
import { useToast } from "../components/Toasts";

const STATUS_STYLE: Record<string, string> = {
  planning: "bg-slate-500/10 text-slate-300 ring-slate-500/25",
  awaiting: "bg-amber-500/10 text-amber-300 ring-amber-500/25",
  running: "bg-sky-500/10 text-sky-300 ring-sky-500/25",
  verifying: "bg-violet-500/10 text-violet-300 ring-violet-500/25",
  paused: "bg-orange-500/10 text-orange-300 ring-orange-500/25",
  done: "bg-emerald-500/10 text-emerald-300 ring-emerald-500/25",
  failed: "bg-red-500/10 text-red-300 ring-red-500/25",
};

const MISSION_STYLE: Record<string, string> = {
  queued: "text-slate-400",
  running: "text-sky-300",
  review: "text-amber-300",
  blocked: "text-orange-300",
  done: "text-emerald-300",
  failed: "text-red-300",
};

const LEVELS: { id: AutonomyLevel; label: string; hint: string }[] = [
  { id: "watch", label: "Watch", hint: "Plan only: the plan and every mission wait for your click, and you merge." },
  { id: "ask", label: "Ask on risk", hint: "Runs everything and asks only on the escalation list: budget passed, a second reject, stuck after retry, a destructive approval, a merge outside the allow list, a done check failing twice." },
  { id: "full", label: "Full", hint: "Like Ask on risk, and also merges outside the allow list once the reviewer approved and CI is green." },
];

const levelLabel = (l: string) => LEVELS.find((x) => x.id === l)?.label ?? l;

const emptyCheck = (): GoalCheck => ({ kind: "command", target: "", repo: null });

/**
 * Work > Goals: the owner states an outcome and how Studio will know it is finished; Studio plans
 * it into missions and runs them at the chosen autonomy level.
 */
export function Goals() {
  const toast = useToast();
  const [rows, setRows] = useState<GoalDetail[] | null>(null);
  const [projects, setProjects] = useState<Project[]>([]);
  const [open, setOpen] = useState(false);
  const [busy, setBusy] = useState(false);
  const [selected, setSelected] = useState<string | null>(null);
  const [outcome, setOutcome] = useState("");
  const [repos, setRepos] = useState<string[]>([]);
  const [checks, setChecks] = useState<GoalCheck[]>([emptyCheck()]);
  const [budget, setBudget] = useState("0");
  const [deadline, setDeadline] = useState("");
  const [level, setLevel] = useState<AutonomyLevel | "">("");
  const [defaults, setDefaults] = useState<Record<string, AutonomyLevel>>({});
  const [asks, setAsks] = useState<Escalation[]>([]);

  async function load() {
    try {
      setRows(await api.goals());
      setAsks(await api.escalations(100));
    } catch (e) {
      toast("error", errorText(e));
      setRows([]);
    }
  }

  useEffect(() => {
    void load();
    api
      .projects()
      .then((p) => setProjects(p.filter((x) => !x.archived)))
      .catch(() => setProjects([]));
    const t = window.setInterval(() => void load(), 15000);
    return () => window.clearInterval(t);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    if (repos.length === 0) {
      setDefaults({});
      return;
    }
    api
      .autonomyDefaults(repos)
      .then(setDefaults)
      .catch(() => setDefaults({}));
  }, [repos]);

  const inherited: AutonomyLevel = useMemo(() => {
    const order: AutonomyLevel[] = ["watch", "ask", "full"];
    const levels = repos.map((r) => defaults[r] ?? "ask");
    if (levels.length === 0) return "ask";
    return levels.reduce((a, b) => (order.indexOf(b) < order.indexOf(a) ? b : a));
  }, [repos, defaults]);

  async function create() {
    const goal: NewGoal = {
      outcome: outcome.trim(),
      repos,
      checks: checks
        .filter((c) => c.target.trim() !== "")
        .map((c) => ({ kind: c.kind, target: c.target.trim(), repo: c.repo || null })),
      budget_usd: Number(budget) || 0,
      deadline: deadline || null,
      autonomy: level || null,
    };
    setBusy(true);
    try {
      const g = await api.createGoal(goal);
      toast("ok", `${g.id} created at ${levelLabel(g.autonomy)}`);
      setOutcome("");
      setRepos([]);
      setChecks([emptyCheck()]);
      setBudget("0");
      setDeadline("");
      setLevel("");
      setOpen(false);
      setSelected(g.id);
      await load();
    } catch (e) {
      toast("error", errorText(e));
    } finally {
      setBusy(false);
    }
  }

  async function changeLevel(id: string, l: AutonomyLevel) {
    try {
      await api.setGoalAutonomy(id, l);
      toast("ok", `${id} is now at ${levelLabel(l)}`);
      await load();
    } catch (e) {
      toast("error", errorText(e));
    }
  }

  async function act(id: string, what: "plan" | "approve") {
    setBusy(true);
    try {
      const g = what === "plan" ? await api.planGoal(id) : await api.approveGoalPlan(id);
      toast("ok", g.note || `${id} is ${g.status}`);
      await load();
    } catch (e) {
      toast("error", errorText(e));
      await load();
    } finally {
      setBusy(false);
    }
  }

  async function reply(e: Escalation, answer: string) {
    setBusy(true);
    try {
      await api.answerEscalation(e.id, answer);
      toast("ok", `${e.id}: ${answer}`);
      await load();
    } catch (err) {
      toast("error", errorText(err));
    } finally {
      setBusy(false);
    }
  }

  async function remove(id: string) {
    try {
      await api.deleteGoal(id);
      toast("ok", `${id} deleted; its vault note stays as the record`);
      setSelected(null);
      await load();
    } catch (e) {
      toast("error", errorText(e));
    }
  }

  const toggleRepo = (slug: string) => setRepos((r) => (r.includes(slug) ? r.filter((x) => x !== slug) : [...r, slug]));
  const setCheck = (i: number, c: Partial<GoalCheck>) => setChecks((all) => all.map((x, j) => (j === i ? { ...x, ...c } : x)));
  const detail = rows?.find((r) => r.goal.id === selected) ?? null;
  const openCount = rows?.filter((r) => r.goal.status !== "done" && r.goal.status !== "failed").length ?? 0;

  return (
    <div className="flex h-full flex-col overflow-y-auto">
      <div className="flex items-center gap-2 border-b border-line px-5 py-3">
        <Target className="size-4 text-emerald-300" />
        <h1 className="text-body font-semibold text-slate-100">Goals</h1>
        <span className="text-label text-slate-500">{rows === null ? "loading" : rows.length === 0 ? "none yet" : `${openCount} open`}</span>
        <div className="ml-auto flex gap-2">
          <Button size="sm" variant="ghost" icon={<RefreshCw className="size-3.5" />} onClick={() => void load()}>
            Refresh
          </Button>
          <Button size="sm" variant={open ? "secondary" : "primary"} icon={open ? <ChevronDown className="size-3.5" /> : <Plus className="size-3.5" />} onClick={() => setOpen((v) => !v)}>
            New goal
          </Button>
        </div>
      </div>

      {open && (
        <Card className="mx-5 mt-4 space-y-3 p-4">
          <label className="block text-label text-slate-400">
            Outcome, in one sentence
            <textarea
              value={outcome}
              onChange={(e) => setOutcome(e.target.value)}
              rows={2}
              placeholder="Add a /probe endpoint that returns ok, with a test, and document it in the README"
              className={`${inputClass} mt-1`}
            />
          </label>
          <div className="text-label text-slate-400">
            Repositories (Hermes projects)
            <div className="mt-1 flex flex-wrap gap-1.5">
              {projects.length === 0 && <span className="text-slate-500">No Hermes projects yet. Add one in Work: Projects.</span>}
              {projects.map((p) => (
                <button
                  key={p.slug}
                  onClick={() => toggleRepo(p.slug)}
                  aria-pressed={repos.includes(p.slug)}
                  className={`h-7 rounded-lg px-2.5 text-small ring-1 transition-colors ${
                    repos.includes(p.slug) ? "bg-emerald-500/10 text-emerald-300 ring-emerald-500/40" : "text-slate-400 ring-line hover:bg-white/[0.04]"
                  }`}
                >
                  {p.name}
                </button>
              ))}
            </div>
          </div>
          <div className="text-label text-slate-400">
            Definition of done: checks Studio runs after the last merge
            <div className="mt-1 space-y-1.5">
              {checks.map((c, i) => (
                <div key={i} className="flex gap-2">
                  <select
                    value={c.kind}
                    onChange={(e) => setCheck(i, { kind: e.target.value as GoalCheck["kind"] })}
                    aria-label="Check kind"
                    className="h-9 w-28 rounded-lg border border-line bg-canvas px-2 text-sm text-slate-200"
                  >
                    <option value="command">Command</option>
                    <option value="url">URL</option>
                    <option value="file">File</option>
                  </select>
                  <input
                    value={c.target}
                    onChange={(e) => setCheck(i, { target: e.target.value })}
                    placeholder={c.kind === "command" ? "npm test" : c.kind === "url" ? "https://example.com/probe" : "docs/PROBE.md"}
                    aria-label="Check target"
                    className={inputClass}
                  />
                  {repos.length > 1 && (
                    <select
                      value={c.repo ?? ""}
                      onChange={(e) => setCheck(i, { repo: e.target.value || null })}
                      aria-label="Repository the check runs in"
                      className="h-9 w-40 rounded-lg border border-line bg-canvas px-2 text-sm text-slate-200"
                    >
                      <option value="">{repos[0]}</option>
                      {repos.slice(1).map((r) => (
                        <option key={r} value={r}>
                          {r}
                        </option>
                      ))}
                    </select>
                  )}
                  <Button size="sm" variant="ghost" aria-label="Remove check" icon={<X className="size-3.5" />} onClick={() => setChecks((all) => all.filter((_, j) => j !== i))} disabled={checks.length === 1} />
                </div>
              ))}
              <Button size="sm" variant="ghost" icon={<Plus className="size-3.5" />} onClick={() => setChecks((all) => [...all, emptyCheck()])}>
                Add check
              </Button>
            </div>
          </div>
          <div className="grid grid-cols-1 gap-3 md:grid-cols-3">
            <label className="text-label text-slate-400" title="Metered spend only (OpenRouter). Flat subscriptions never count. 0 means no metered spend is allowed.">
              Metered budget (USD)
              <input type="number" min={0} step={0.5} value={budget} onChange={(e) => setBudget(e.target.value)} className={`${inputClass} mt-1`} />
            </label>
            <label className="text-label text-slate-400" title="Past the deadline the digest marks the goal late. It never stops work by itself.">
              Deadline (optional)
              <input type="date" value={deadline} onChange={(e) => setDeadline(e.target.value)} className={`${inputClass} mt-1`} />
            </label>
            <label className="text-label text-slate-400">
              Autonomy
              <select value={level} onChange={(e) => setLevel(e.target.value as AutonomyLevel | "")} className="mt-1 h-9 w-full rounded-lg border border-line bg-canvas px-2 text-sm text-slate-200">
                <option value="">Repository default ({levelLabel(inherited)})</option>
                {LEVELS.map((l) => (
                  <option key={l.id} value={l.id}>
                    {l.label}
                  </option>
                ))}
              </select>
            </label>
          </div>
          <p className="text-label text-slate-500">{LEVELS.find((l) => l.id === (level || inherited))?.hint}</p>
          <div className="flex justify-end">
            <Button variant="primary" busy={busy} disabled={!outcome.trim() || repos.length === 0 || !checks.some((c) => c.target.trim())} icon={<Flag className="size-4" />} onClick={() => void create()}>
              Create goal
            </Button>
          </div>
        </Card>
      )}

      {asks.some((a) => a.status === "open") && (
        <Card className="mx-5 mt-4 space-y-2 border-amber-500/30 p-4">
          <div className="flex items-center gap-2">
            <Flag className="size-4 text-amber-300" />
            <h2 className="text-small font-semibold text-slate-100">Waiting on you</h2>
            <span className="text-label text-slate-500">Studio only asks for these; with no answer by the deadline it takes the safe answer.</span>
          </div>
          {asks
            .filter((a) => a.status === "open")
            .map((a) => (
              <EscalationRow key={a.id} e={a} busy={busy} onAnswer={(ans) => void reply(a, ans)} />
            ))}
        </Card>
      )}

      <div className="grid min-h-0 flex-1 grid-cols-1 gap-4 p-5 lg:grid-cols-[minmax(0,22rem)_minmax(0,1fr)]">
        <div className="space-y-2">
          {rows !== null && rows.length === 0 && <Empty icon={<Target className="size-5" />} title="No goals yet" hint="State an outcome and how Studio will know it is done. Studio plans the missions and runs them." />}
          {rows?.map(({ goal, missions }) => {
            const done = missions.filter((m) => m.status === "done").length;
            return (
              <button
                key={goal.id}
                onClick={() => setSelected(goal.id)}
                className={`block w-full rounded-xl p-3 text-left ring-1 transition-colors ${selected === goal.id ? "bg-white/[0.05] ring-emerald-500/40" : "ring-line hover:bg-white/[0.03]"}`}
              >
                <div className="flex items-center gap-2">
                  <span className="font-mono text-label text-slate-500">{goal.id}</span>
                  <Badge className={STATUS_STYLE[goal.status] ?? ""}>{goal.status}</Badge>
                  <span className="ml-auto text-label text-slate-500">{levelLabel(goal.autonomy)}</span>
                </div>
                <div className="mt-1 line-clamp-2 text-small text-slate-200">{goal.outcome}</div>
                <div className="mt-1 text-label text-slate-500">
                  {missions.length === 0 ? "no missions yet" : `${done} of ${missions.length} missions done`} · {goal.repos.join(", ")}
                </div>
              </button>
            );
          })}
        </div>

        <div>
          {detail ? (
            <GoalPanel
              detail={detail}
              asks={asks.filter((a) => a.goal_id === detail.goal.id)}
              busy={busy}
              onLevel={(l) => void changeLevel(detail.goal.id, l)}
              onPlan={() => void act(detail.goal.id, "plan")}
              onApprove={() => void act(detail.goal.id, "approve")}
              onDelete={() => void remove(detail.goal.id)}
            />
          ) : (
            rows !== null && rows.length > 0 && <Empty icon={<ChevronRight className="size-5" />} title="Pick a goal" hint="Its checks, missions and spend show here." />
          )}
        </div>
      </div>
    </div>
  );
}

function GoalPanel({
  detail,
  asks,
  busy,
  onLevel,
  onPlan,
  onApprove,
  onDelete,
}: {
  detail: GoalDetail;
  asks: Escalation[];
  busy: boolean;
  onLevel: (l: AutonomyLevel) => void;
  onPlan: () => void;
  onApprove: () => void;
  onDelete: () => void;
}) {
  const { goal, missions, spent_usd } = detail;
  const planFailed = goal.status === "planning" && goal.note.startsWith("The planner failed");
  const planning = goal.status === "planning" && !planFailed;
  const finished = goal.status === "done" || goal.status === "failed";
  const inFlight = missions.some((m) => m.status === "running" || m.status === "review");
  return (
    <Card className="space-y-4 p-4">
      <div>
        <div className="flex items-center gap-2">
          <span className="font-mono text-label text-slate-500">{goal.id}</span>
          <Badge className={STATUS_STYLE[goal.status] ?? ""}>{goal.status}</Badge>
          {goal.deadline && <span className="text-label text-slate-500">due {goal.deadline}</span>}
        </div>
        <h2 className="mt-1 text-body font-semibold text-slate-100">{goal.outcome}</h2>
        {goal.note && <p className="mt-1 text-small text-slate-400">{goal.note}</p>}
        {planning && <p className="mt-1 text-label text-slate-500">Hermes splits the goal into missions; this takes about half a minute.</p>}
        {(planFailed || goal.status === "awaiting") && (
          <div className="mt-2 flex gap-2">
            {planFailed && (
              <Button size="sm" variant="primary" busy={busy} icon={<RefreshCw className="size-3.5" />} onClick={onPlan}>
                Plan again
              </Button>
            )}
            {goal.status === "awaiting" && (
              <Button size="sm" variant="primary" busy={busy} icon={<CheckCircle2 className="size-3.5" />} onClick={onApprove} title="Let the goal run. At Watch each mission still waits for its Start click in Mission Control.">
                Approve plan
              </Button>
            )}
          </div>
        )}
      </div>

      <div className="flex flex-wrap items-center gap-3 text-label text-slate-400">
        <label className="flex items-center gap-2">
          Autonomy
          <select
            value={goal.autonomy}
            disabled={finished}
            onChange={(e) => onLevel(e.target.value as AutonomyLevel)}
            title={LEVELS.find((l) => l.id === goal.autonomy)?.hint}
            className="h-7 rounded-lg border border-line bg-canvas px-2 text-small text-slate-200"
          >
            {LEVELS.map((l) => (
              <option key={l.id} value={l.id}>
                {l.label}
              </option>
            ))}
          </select>
        </label>
        <span>
          Spent <span className="text-slate-200">${spent_usd.toFixed(2)}</span>
          {goal.budget_usd > 0 ? ` of $${goal.budget_usd.toFixed(2)} metered` : " (no metered budget)"}
        </span>
        <span>Repositories: {goal.repos.join(", ")}</span>
      </div>

      <section>
        <h3 className="text-label font-semibold uppercase tracking-wide text-slate-500">Definition of done</h3>
        <ul className="mt-1 space-y-1">
          {goal.checks.map((c, i) => (
            <li key={i} className="flex items-center gap-2 text-small text-slate-300">
              <CheckCircle2 className="size-3.5 text-slate-500" />
              <span className="text-slate-500">{c.kind}</span>
              <code className="font-mono text-slate-200">{c.target}</code>
              {c.repo && <span className="text-slate-500">in {c.repo}</span>}
            </li>
          ))}
        </ul>
      </section>

      <section>
        <h3 className="text-label font-semibold uppercase tracking-wide text-slate-500">Missions</h3>
        {missions.length === 0 ? (
          <p className="mt-1 text-small text-slate-500">{goal.status === "planning" ? "The planner has not split this goal yet." : "None."}</p>
        ) : (
          <ul className="mt-1 divide-y divide-line">
            {missions.map((m) => (
              <li key={m.id} className="flex items-start gap-2 py-2 text-small">
                <span className="font-mono text-label text-slate-500">{m.id}</span>
                <span className={`w-16 shrink-0 ${MISSION_STYLE[m.status] ?? ""}`}>{m.status}</span>
                <span className="min-w-0 flex-1 text-slate-200">{m.goal}</span>
                <span className="text-label text-slate-500">{m.agent}</span>
                {m.pr_url && (
                  <a href={m.pr_url} target="_blank" rel="noreferrer" className="text-label text-sky-300 hover:underline">
                    PR
                  </a>
                )}
              </li>
            ))}
          </ul>
        )}
      </section>

      {asks.length > 0 && (
        <section>
          <h3 className="text-label font-semibold uppercase tracking-wide text-slate-500">Escalations</h3>
          <ul className="mt-1 space-y-1">
            {asks.map((a) => (
              <li key={a.id} className="text-small text-slate-300">
                <span className="font-mono text-label text-slate-500">{a.id}</span> {a.question}{" "}
                <span className="text-slate-500">
                  {a.status === "open" ? "open" : `${a.answer} (${a.answered_by === "timeout" ? "no answer, safe default" : `answered from ${a.answered_by}`})`}
                </span>
              </li>
            ))}
          </ul>
        </section>
      )}

      {goal.report && (
        <section>
          <h3 className="text-label font-semibold uppercase tracking-wide text-slate-500">Report</h3>
          <pre className="mt-1 whitespace-pre-wrap text-small text-slate-300">{goal.report}</pre>
        </section>
      )}

      <div className="flex justify-end">
        <Button size="sm" variant="danger" icon={<Trash2 className="size-3.5" />} disabled={inFlight} title={inFlight ? "A mission is running or in review" : "Delete the goal and its missions; the vault note stays"} onClick={onDelete}>
          Delete goal
        </Button>
      </div>
    </Card>
  );
}

const KIND_LABEL: Record<string, string> = {
  budget: "Budget passed",
  second_reject: "Rejected twice",
  stuck: "Stuck after retry",
  destructive: "Destructive approval",
  merge: "Merge outside the allow list",
  done_check: "Done check failed twice",
};

function EscalationRow({ e, busy, onAnswer }: { e: Escalation; busy: boolean; onAnswer: (answer: string) => void }) {
  const left = Math.max(0, e.deadline_at - Math.floor(Date.now() / 1000));
  const mins = Math.round(left / 60);
  return (
    <div className="flex flex-wrap items-center gap-2 rounded-lg bg-white/[0.02] p-2 ring-1 ring-line">
      <span className="font-mono text-label text-slate-500">{e.id}</span>
      <Badge className="bg-amber-500/10 text-amber-300 ring-amber-500/25">{KIND_LABEL[e.kind] ?? e.kind}</Badge>
      <span className="min-w-0 flex-1 text-small text-slate-200">{e.question}</span>
      <span className="text-label text-slate-500" title="With no answer by then, Studio takes the safe answer">
        {mins >= 60 ? `${Math.round(mins / 60)} h` : `${mins} min`} left, then {e.safe_answer}
      </span>
      {e.options.map((o) => (
        <Button key={o} size="sm" variant={o === e.safe_answer ? "secondary" : "primary"} busy={busy} onClick={() => onAnswer(o)}>
          {o}
        </Button>
      ))}
    </div>
  );
}
