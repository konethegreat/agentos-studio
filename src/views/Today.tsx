import { useCallback, useEffect, useMemo, useState } from "react";
import { ArrowRight, CircleDollarSign, FileText, Flag, Pause, Play, RefreshCw, Sunrise, UserCheck, Users } from "lucide-react";
import {
  api,
  errorText,
  type Escalation,
  type FleetPause,
  type GoalDetail,
  type Mission,
  type WorkerInfo,
} from "../lib/api";
import { Markdown } from "../components/Markdown";
import { useToast } from "../components/Toasts";
import { Badge, Button, Card, Empty } from "../components/ui";
import { AGENT_COLOR, AGENT_LABEL, relativeTime } from "../lib/format";

const GOAL_STATUS: Record<string, string> = {
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

const KIND_LABEL: Record<string, string> = {
  budget: "Budget passed",
  second_reject: "Rejected twice",
  stuck: "Stuck after retry",
  destructive: "Destructive approval",
  merge: "Merge outside the allow list",
  done_check: "Done check failed twice",
};

const IN_PROGRESS = ["planning", "running", "verifying", "paused"];

const usd = (n: number) => `$${n.toFixed(2)}`;
const at = (unix: number) => relativeTime(new Date(unix * 1000).toISOString());

function digestParts(text: string | null): { title: string; body: string } {
  if (!text) return { title: "", body: "" };
  const [first, ...rest] = text.split("\n");
  return { title: first.replace(/^#\s*/, "").trim(), body: rest.join("\n").trim() };
}

function go(view: string) {
  window.dispatchEvent(new CustomEvent<string>("agentos:go", { detail: view }));
}

function deadlineText(e: Escalation): string {
  const left = Math.max(0, e.deadline_at - Math.floor(Date.now() / 1000));
  const mins = Math.round(left / 60);
  const when = mins >= 60 ? `${Math.round(mins / 60)} h` : `${mins} min`;
  return `${when} left, then ${e.safe_answer}`;
}

/**
 * Today: the home screen. What waits on the owner comes first, in the digest's order (open
 * escalations, then goals whose plan needs approval), then goals in progress, the agents working
 * now, metered spend and the last digest.
 */
export function Today() {
  const toast = useToast();
  const [rows, setRows] = useState<GoalDetail[] | null>(null);
  const [asks, setAsks] = useState<Escalation[]>([]);
  const [missions, setMissions] = useState<Mission[]>([]);
  const [workers, setWorkers] = useState<WorkerInfo[]>([]);
  const [pause, setPause] = useState<FleetPause | null>(null);
  const [digest, setDigest] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [updatedAt, setUpdatedAt] = useState(0);

  const load = useCallback(async () => {
    try {
      const [g, e, m, w, p, d] = await Promise.all([
        api.goals(),
        api.escalations(100).catch(() => [] as Escalation[]),
        api.missions().catch(() => [] as Mission[]),
        api.workers().catch(() => [] as WorkerInfo[]),
        api.fleetPauseState().catch(() => null as FleetPause | null),
        api.digestLatest().catch(() => null as string | null),
      ]);
      setRows(g);
      setAsks(e);
      setMissions(m);
      setWorkers(w);
      setPause(p);
      setDigest(d);
      setUpdatedAt(Date.now());
    } catch (e) {
      toast("error", errorText(e));
      setRows((r) => r ?? []);
    }
  }, [toast]);

  useEffect(() => {
    void load();
    const t = window.setInterval(() => void load(), 15000);
    return () => window.clearInterval(t);
  }, [load]);

  async function reply(e: Escalation, answer: string) {
    setBusy(e.id);
    try {
      await api.answerEscalation(e.id, answer);
      toast("ok", `${e.id}: ${answer}`);
    } catch (err) {
      toast("error", errorText(err));
    } finally {
      setBusy(null);
      await load();
    }
  }

  async function approve(id: string) {
    setBusy(id);
    try {
      const g = await api.approveGoalPlan(id);
      toast("ok", g.note || `${id} is ${g.status}`);
    } catch (e) {
      toast("error", errorText(e));
    } finally {
      setBusy(null);
      await load();
    }
  }

  async function resume() {
    setBusy("resume");
    try {
      await api.fleetResume();
      toast("ok", "Fleet resumed");
    } catch (e) {
      toast("error", errorText(e));
    } finally {
      setBusy(null);
      await load();
    }
  }

  const openAsks = useMemo(() => asks.filter((a) => a.status === "open"), [asks]);
  const awaiting = useMemo(() => (rows ?? []).filter((r) => r.goal.status === "awaiting"), [rows]);
  const active = useMemo(() => (rows ?? []).filter((r) => IN_PROGRESS.includes(r.goal.status)), [rows]);
  const running = useMemo(() => missions.filter((m) => m.status === "running" || m.status === "review"), [missions]);
  const spent = useMemo(() => (rows ?? []).reduce((n, r) => n + r.spent_usd, 0), [rows]);
  const budgets = useMemo(() => (rows ?? []).reduce((n, r) => n + r.goal.budget_usd, 0), [rows]);
  const waiting = openAsks.length + awaiting.length;
  const digestText = digestParts(digest);

  return (
    <div className="flex h-full flex-col overflow-y-auto">
      <div className="flex flex-wrap items-center gap-2 border-b border-line px-5 py-3">
        <Sunrise className="size-4 text-amber-300" />
        <h1 className="text-body font-semibold text-slate-100">Today</h1>
        <span className="text-label text-slate-500">{new Date().toLocaleDateString(undefined, { weekday: "long", day: "numeric", month: "long" })}</span>
        {waiting > 0 && <Badge className="bg-amber-500/15 text-amber-200 ring-amber-400/25">{waiting} waiting on you</Badge>}
        <div className="ml-auto flex items-center gap-2">
          <span className="text-label text-slate-600">{updatedAt ? `checked ${relativeTime(new Date(updatedAt).toISOString())}` : "reading"}</span>
          <Button size="sm" variant="ghost" icon={<RefreshCw className="size-3.5" />} onClick={() => void load()}>
            Refresh
          </Button>
        </div>
      </div>

      {pause?.paused && (
        <Card className="mx-5 mt-4 border-orange-500/30 p-3">
          <div className="flex flex-wrap items-center gap-2">
            <Pause className="size-4 text-orange-300" />
            <span className="text-small font-semibold text-slate-100">Fleet paused</span>
            {pause.since && <span className="text-label text-slate-500">since {at(pause.since)}</span>}
            <span className="min-w-0 flex-1 text-small text-slate-300">{pause.reason || "No reason given."}</span>
            {pause.hermes_paused && <Badge className="bg-orange-500/15 text-orange-200 ring-orange-400/25">Hermes is stopped too</Badge>}
            <Button size="sm" variant="secondary" icon={<Play className="size-3.5" />} busy={busy === "resume"} onClick={() => void resume()}>
              Resume
            </Button>
          </div>
        </Card>
      )}

      <div className="grid grid-cols-1 gap-4 p-5 xl:grid-cols-[minmax(0,1fr)_22rem]">
        <div className="min-w-0 space-y-4">
          <Card className={`p-0 ${waiting > 0 ? "border-amber-500/30" : ""}`}>
            <div className="flex flex-wrap items-center gap-2 border-b border-line px-4 py-3">
              <UserCheck className={`size-4 ${waiting > 0 ? "text-amber-300" : "text-emerald-300"}`} />
              <h2 className="text-sm font-semibold text-slate-100">Waiting on you</h2>
              <span className="text-label text-slate-500">Questions and plans stop work until you answer.</span>
            </div>
            {rows === null ? (
              <div className="px-4 py-3 text-small text-slate-500">Reading goals and escalations...</div>
            ) : waiting === 0 ? (
              <div className="flex items-center gap-2 px-4 py-3 text-small text-emerald-300">
                <Flag className="size-3.5" />
                Nothing waits on you. Everything either runs or is finished.
              </div>
            ) : (
              <>
                {openAsks.map((e) => (
                  <div key={e.id} className="border-t border-line px-4 py-2.5">
                    <div className="flex flex-wrap items-center gap-2">
                      <span className="font-mono text-label text-slate-500">{e.id}</span>
                      <Badge className="bg-amber-500/10 text-amber-300 ring-amber-500/25">{KIND_LABEL[e.kind] ?? e.kind}</Badge>
                      <span className="min-w-0 flex-1 text-small text-slate-200">{e.question}</span>
                      <span className="text-label text-slate-500" title="With no answer by then, Studio takes the safe answer">
                        {deadlineText(e)}
                      </span>
                      {e.options.map((o) => (
                        <Button
                          key={o}
                          size="sm"
                          variant={o === e.safe_answer ? "secondary" : "primary"}
                          busy={busy === e.id}
                          onClick={() => void reply(e, o)}
                        >
                          {o}
                        </Button>
                      ))}
                    </div>
                  </div>
                ))}
                {awaiting.map(({ goal }) => (
                  <div key={goal.id} className="border-t border-line px-4 py-2.5">
                    <div className="flex flex-wrap items-center gap-2">
                      <span className="font-mono text-label text-slate-500">{goal.id}</span>
                      <Badge className={GOAL_STATUS.awaiting}>plan ready</Badge>
                      <span className="min-w-0 flex-1 text-small text-slate-200">{goal.outcome}</span>
                      <span className="text-label text-slate-500">{goal.repos.join(", ") || "no repositories"}</span>
                      <Button
                        size="sm"
                        variant="primary"
                        icon={<Play className="size-3.5" />}
                        busy={busy === goal.id}
                        onClick={() => void approve(goal.id)}
                      >
                        Approve plan
                      </Button>
                      <Button size="sm" variant="ghost" icon={<ArrowRight className="size-3.5" />} onClick={() => go("goals")}>
                        Open
                      </Button>
                    </div>
                    {goal.note && <p className="mt-0.5 text-label text-slate-500">{goal.note}</p>}
                  </div>
                ))}
              </>
            )}
          </Card>

          <Card className="p-0">
            <div className="flex flex-wrap items-center gap-2 border-b border-line px-4 py-3">
              <Flag className="size-4 text-sky-300" />
              <h2 className="text-sm font-semibold text-slate-100">Goals in progress</h2>
              <span className="text-label text-slate-500">Outcomes Studio is planning, running or checking.</span>
            </div>
            {rows === null ? (
              <div className="px-4 py-3 text-small text-slate-500">Reading goals...</div>
            ) : active.length === 0 ? (
              <div className="px-4 py-4">
                <Empty
                  icon={<Flag className="size-5" />}
                  title="No goals in progress"
                  hint="State an outcome and how Studio will know it is done; it plans the missions and runs them."
                />
                <div className="mt-2 flex justify-center">
                  <Button size="sm" variant="secondary" icon={<ArrowRight className="size-3.5" />} onClick={() => go("goals")}>
                    Open Goals
                  </Button>
                </div>
              </div>
            ) : (
              active.map((r) => {
                const done = r.missions.filter((m) => m.status === "done").length;
                const total = r.missions.length;
                const pct = total > 0 ? Math.round((done / total) * 100) : 0;
                return (
                  <button
                    key={r.goal.id}
                    onClick={() => go("goals")}
                    className="block w-full border-t border-line px-4 py-2.5 text-left hover:bg-white/[0.03]"
                  >
                    <div className="flex flex-wrap items-center gap-2">
                      <span className="font-mono text-label text-slate-500">{r.goal.id}</span>
                      <Badge className={GOAL_STATUS[r.goal.status] ?? ""}>{r.goal.status}</Badge>
                      <span className="min-w-0 flex-1 text-small text-slate-200">{r.goal.outcome}</span>
                      <span className="text-label text-slate-500">{r.goal.autonomy}</span>
                      <span className="text-label text-slate-500">
                        {done}/{total} missions
                      </span>
                      <span className="text-label text-slate-400">
                        {usd(r.spent_usd)}
                        {r.goal.budget_usd > 0 ? ` of ${usd(r.goal.budget_usd)}` : ""}
                      </span>
                    </div>
                    <div className="mt-1.5 flex items-center gap-2">
                      <div className="h-1 w-40 overflow-hidden rounded bg-white/[0.06]">
                        <div className="h-full bg-emerald-400/70" style={{ width: `${pct}%` }} />
                      </div>
                      <span className="text-label text-slate-600">{r.goal.repos.join(", ")}</span>
                      {r.goal.note && <span className="truncate text-label text-slate-600">{r.goal.note}</span>}
                    </div>
                  </button>
                );
              })
            )}
          </Card>

          <Card className="p-0">
            <div className="flex flex-wrap items-center gap-2 border-b border-line px-4 py-3">
              <Users className="size-4 text-violet-300" />
              <h2 className="text-sm font-semibold text-slate-100">Running agents</h2>
              <span className="text-label text-slate-500">Missions being worked on now, and the workers this machine started.</span>
            </div>
            {rows === null ? (
              <div className="px-4 py-3 text-small text-slate-500">Reading missions and workers...</div>
            ) : workers.length === 0 && running.length === 0 ? (
              <div className="px-4 py-3 text-small text-slate-500">Nothing is running.</div>
            ) : (
              <>
                {workers.map((w) => (
                  <div key={w.task_id} className="flex flex-wrap items-center gap-2 border-t border-line px-4 py-2.5">
                    <Badge className={AGENT_COLOR[w.agent] ?? ""}>{AGENT_LABEL[w.agent] ?? w.agent}</Badge>
                    <span className="font-mono text-label text-slate-300">{w.task_id}</span>
                    <span className="text-label text-slate-500">worker, pid {w.pid}</span>
                    <span className="ml-auto text-label text-slate-500">started {relativeTime(w.started_at)}</span>
                  </div>
                ))}
                {running.map((m) => (
                  <div key={m.id} className="flex flex-wrap items-center gap-2 border-t border-line px-4 py-2.5">
                    <Badge className={AGENT_COLOR[m.agent] ?? ""}>{AGENT_LABEL[m.agent] ?? m.agent}</Badge>
                    <span className="font-mono text-label text-slate-500">{m.id}</span>
                    <span className={`text-label ${MISSION_STYLE[m.status] ?? ""}`}>{m.status}</span>
                    <span className="min-w-0 flex-1 truncate text-small text-slate-200">{m.goal}</span>
                    <span className="text-label text-slate-500">{m.repo}</span>
                    <span className="text-label text-slate-400">
                      {usd(m.spent_usd)}
                      {m.budget_usd > 0 ? ` of ${usd(m.budget_usd)}` : ""}
                    </span>
                    {m.pr_url && (
                      <a href={m.pr_url} target="_blank" rel="noreferrer" className="text-label text-sky-300 hover:underline">
                        PR
                      </a>
                    )}
                  </div>
                ))}
              </>
            )}
          </Card>
        </div>

        <div className="min-w-0 space-y-4">
          <Card className="p-0">
            <div className="flex items-center gap-2 border-b border-line px-4 py-3">
              <CircleDollarSign className="size-4 text-emerald-300" />
              <h2 className="text-sm font-semibold text-slate-100">Spend</h2>
            </div>
            <div className="px-4 py-3">
              <div className="text-2xl font-semibold text-slate-100">{usd(spent)}</div>
              <p className="mt-0.5 text-label text-slate-500">
                {rows?.length
                  ? `Metered spend across the ${rows.length} goal${rows.length === 1 ? "" : "s"} in this list. Flat subscriptions are not counted.`
                  : "Metered spend across goals. Flat subscriptions are not counted. Nothing has run yet."}
              </p>
              {budgets > 0 && (
                <p className="mt-1 text-label text-slate-500">
                  Budgets total {usd(budgets)}. Studio asks before a goal passes its own budget.
                </p>
              )}
              {(rows ?? []).filter((r) => r.spent_usd > 0).length > 0 && (
                <div className="mt-2 space-y-1 border-t border-line pt-2">
                  {(rows ?? [])
                    .filter((r) => r.spent_usd > 0)
                    .map((r) => (
                      <div key={r.goal.id} className="flex items-center gap-2 text-label">
                        <span className="min-w-0 flex-1 truncate text-slate-300">{r.goal.outcome}</span>
                        <span className="font-mono text-slate-400">{usd(r.spent_usd)}</span>
                      </div>
                    ))}
                </div>
              )}
            </div>
          </Card>

          <Card className="p-0">
            <div className="flex flex-wrap items-center gap-2 border-b border-line px-4 py-3">
              <FileText className="size-4 text-slate-400" />
              <h2 className="text-sm font-semibold text-slate-100">Last digest</h2>
              <span className="text-label text-slate-500">Newest note in 04_Agent_Engine/digests.</span>
            </div>
            {digest ? (
              <>
                {digestText.title && (
                  <div className="border-b border-line px-4 py-2 text-label text-slate-400">{digestText.title}</div>
                )}
                <div className="max-h-80 overflow-y-auto px-4 py-3 text-small">
                  <Markdown text={digestText.body} workingDir={null} />
                </div>
              </>
            ) : (
              <div className="px-4 py-3 text-small text-slate-500">
                No digest yet. Hermes writes one each morning and evening; the schedule is on Work: Goals.
              </div>
            )}
          </Card>
        </div>
      </div>
    </div>
  );
}
