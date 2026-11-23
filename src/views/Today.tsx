import { useCallback, useEffect, useMemo, useState } from "react";
import { ArrowRight, BookOpenCheck, Bot, CircleDollarSign, FileText, Flag, Pause, Play, RefreshCw, Stethoscope, Sunrise, Users } from "lucide-react";
import {
  api,
  errorText,
  type Escalation,
  type FleetDoctorReport,
  type FleetPause,
  type GoalDetail,
  type Learned,
  type Mission,
  type WorkerInfo,
} from "../lib/api";
import { studio, type OrchestratorStatus } from "../lib/studio";
import { useUpdates } from "../components/Updates";
import { Markdown } from "../components/Markdown";
import { useToast } from "../components/Toasts";
import { Badge, Button, Card, Empty } from "../components/ui";
import { AGENT_COLOR, AGENT_LABEL, relativeTime } from "../lib/format";
import { useRecordTarget, type RecordTarget } from "../lib/nav";

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

function OrchStat({ label, value, tone }: { label: string; value: number; tone: string }) {
  return (
    <div className="rounded-lg border border-line bg-canvas/60 px-3 py-2">
      <div className={`text-sm font-semibold ${tone}`}>{value}</div>
      <div className="text-label text-slate-500">{label}</div>
    </div>
  );
}

/**
 * Today: the home screen. The orchestrator card says whether the fleet is running and whether
 * anything needs the owner, then goals in progress, the agents working now, metered spend and the
 * last digest.
 */
export function Today({ target = null }: { target?: RecordTarget | null }) {
  const toast = useToast();
  const updates = useUpdates();
  const source = useRecordTarget(target, api.escalationDetail);
  const refreshSource = source.refresh;
  const [rows, setRows] = useState<GoalDetail[] | null>(null);
  const [asks, setAsks] = useState<Escalation[]>([]);
  const [missions, setMissions] = useState<Mission[]>([]);
  const [workers, setWorkers] = useState<WorkerInfo[]>([]);
  const [pause, setPause] = useState<FleetPause | null>(null);
  const [digest, setDigest] = useState<string | null>(null);
  const [digestTimes, setDigestTimes] = useState("");
  const [weeklyShip, setWeeklyShip] = useState<number | null>(null);
  const [orch, setOrch] = useState<OrchestratorStatus | null>(null);
  const [learned, setLearned] = useState<Learned[]>([]);
  const [doctor, setDoctor] = useState<FleetDoctorReport | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [updatedAt, setUpdatedAt] = useState(0);

  const load = useCallback(async () => {
    refreshSource();
    try {
      const [g, e, m, w, p, d, ds, sb, o, l, fd] = await Promise.all([
        api.goals(),
        api.escalations(100).catch(() => [] as Escalation[]),
        api.missions().catch(() => [] as Mission[]),
        api.workers().catch(() => [] as WorkerInfo[]),
        api.fleetPauseState().catch(() => null as FleetPause | null),
        api.digestLatest().catch(() => null as string | null),
        api.digestSettings().catch(() => null),
        api.selfBuildStatus().catch(() => null),
        studio.orchestrator.status().catch(() => null as OrchestratorStatus | null),
        api.playbookLearned(24).catch(() => [] as Learned[]),
        api.fleetDoctorStatus().catch(() => null as FleetDoctorReport | null),
      ]);
      setRows(g);
      setAsks(e);
      setMissions(m);
      setWorkers(w);
      setPause(p);
      setDigest(d);
      setDigestTimes(ds?.times ?? "");
      setWeeklyShip(sb?.last_weekly_ship_at ?? null);
      setOrch(o);
      setLearned(l);
      setDoctor(fd);
      setUpdatedAt(Date.now());
    } catch (e) {
      toast("error", errorText(e));
      setRows((r) => r ?? []);
    }
  }, [toast, refreshSource]);

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

  async function runDoctor() {
    setBusy("doctor");
    try {
      const rep = await api.fleetDoctorRunNow();
      setDoctor(rep);
      toast(rep.lastError ? "error" : "ok", rep.lastError ?? "Fleet doctor check complete");
    } catch (e) {
      toast("error", errorText(e));
    } finally {
      setBusy(null);
    }
  }

  async function toggleOrchestrator() {
    if (!orch) return;
    setBusy("orchestrator");
    try {
      setOrch(await studio.orchestrator.set(!orch.enabled));
      toast("ok", orch.enabled ? "Orchestrator off. Nothing is delegated or auto-answered." : "Orchestrator on.");
    } catch (e) {
      toast("error", errorText(e));
    } finally {
      setBusy(null);
    }
  }

  const active = useMemo(() => (rows ?? []).filter((r) => IN_PROGRESS.includes(r.goal.status)), [rows]);
  const running = useMemo(() => missions.filter((m) => m.status === "running" || m.status === "review"), [missions]);
  const spent = useMemo(() => (rows ?? []).reduce((n, r) => n + r.spent_usd, 0), [rows]);
  const budgets = useMemo(() => (rows ?? []).reduce((n, r) => n + r.goal.budget_usd, 0), [rows]);
  const needsYou = useMemo(() => {
    if (!orch) return [];
    return orch.needs_you.map((id) => ({ id, question: asks.find((a) => a.id === id)?.question ?? "Open in Goals to see the question." }));
  }, [orch, asks]);
  const digestText = digestParts(digest);

  return (
    <div className="flex h-full flex-col overflow-y-auto">
      <div className="flex flex-wrap items-center gap-2 border-b border-line px-5 py-3">
        <Sunrise className="size-4 text-amber-300" />
        <h1 className="text-body font-semibold text-slate-100">Today</h1>
        <span className="text-label text-slate-500">{new Date().toLocaleDateString(undefined, { weekday: "long", day: "numeric", month: "long" })}</span>
        <div className="ml-auto flex items-center gap-2">
          <span className="text-label text-slate-600">{updatedAt ? `checked ${relativeTime(new Date(updatedAt).toISOString())}` : "reading"}</span>
          <Button size="sm" variant="ghost" icon={<RefreshCw className="size-3.5" />} onClick={() => void load()}>
            Refresh
          </Button>
        </div>
      </div>

      {target && (
        <div ref={source.ref} tabIndex={-1} className="mx-5 mt-4 rounded-xl border border-emerald-500/30 bg-emerald-500/5 p-4 text-small outline-none focus:ring-2 focus:ring-emerald-500/40">
          <div className="flex flex-wrap items-center gap-2"><span className="font-mono text-label text-slate-400">{target.id}</span><span className="text-label text-slate-500">Opened from a notification</span></div>
          {source.loading ? <p role="status">Loading question...</p> : source.error ? (
            <div role="alert"><p>Could not load this question: {source.error}</p><Button size="sm" variant="ghost" onClick={source.refresh}>Retry</Button></div>
          ) : source.record ? (
            <>
              <p className="mt-2 whitespace-pre-wrap break-words text-slate-200">{source.record.question}</p>
              <p className="mt-1 text-label text-slate-500">{source.record.status}{source.record.answer ? `: ${source.record.answer}` : ""}</p>
              {source.record.status === "open" && <div className="mt-3 flex flex-wrap gap-2">{source.record.options.map((option) => <Button key={option} size="sm" variant={option === source.record?.safe_answer ? "secondary" : "primary"} busy={busy === source.record?.id} onClick={() => void reply(source.record!, option)}>{option}</Button>)}</div>}
            </>
          ) : <p>This question no longer exists.</p>}
        </div>
      )}

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
          <Card className={`p-0 ${needsYou.length > 0 ? "border-amber-500/30" : ""}`}>
            <div className="flex flex-wrap items-center gap-2 border-b border-line px-4 py-3">
              <Bot className={`size-4 ${needsYou.length > 0 ? "text-amber-300" : "text-emerald-300"}`} />
              <h2 className="text-sm font-semibold text-slate-100">Orchestrator</h2>
              <Badge
                className={
                  orch
                    ? orch.enabled
                      ? orch.paused
                        ? "bg-orange-500/10 text-orange-300 ring-orange-500/25"
                        : "bg-emerald-500/10 text-emerald-300 ring-emerald-500/25"
                      : "bg-white/5 text-slate-400 ring-white/10"
                    : "bg-white/5 text-slate-400 ring-white/10"
                }
              >
                {orch ? (orch.enabled ? (orch.paused ? "Paused with the fleet" : "Running") : "Off") : "Reading"}
              </Badge>
              <span className="text-label text-slate-500">Delegates work, unsticks stalled missions and answers routine questions without you.</span>
              {orch && (
                <div className="ml-auto">
                  <Button size="sm" variant="ghost" busy={busy === "orchestrator"} onClick={() => void toggleOrchestrator()}>
                    {orch.enabled ? "Turn off" : "Turn on"}
                  </Button>
                </div>
              )}
            </div>
            <div className="space-y-3 px-4 py-3">
              {orch === null ? (
                <div className="text-small text-slate-500">Reading the orchestrator state...</div>
              ) : !orch.enabled ? (
                <div className="text-small text-amber-300">
                  The orchestrator is off. Missions still run when you start them, but nothing is routed, unstuck or auto-answered.
                </div>
              ) : needsYou.length > 0 ? (
                <div className="space-y-1.5">
                  {needsYou.map((n) => (
                    <div key={n.id} className="flex flex-wrap items-center gap-2 text-small">
                      <Badge className="bg-amber-500/10 text-amber-300 ring-amber-500/25">Needs you</Badge>
                      <span className="min-w-0 flex-1 text-slate-200">{n.question}</span>
                      <Button size="sm" variant="secondary" icon={<ArrowRight className="size-3.5" />} onClick={() => go("goals")}>
                        Answer
                      </Button>
                    </div>
                  ))}
                  <p className="text-label text-slate-500">Only production data, credentials and money reach you. Answering is the one job left.</p>
                </div>
              ) : (
                <div className="flex items-center gap-2 text-small text-emerald-300">
                  <Flag className="size-3.5" />
                  Nothing needs you. The orchestrator is running the fleet.
                </div>
              )}
              <div className="grid grid-cols-2 gap-2 sm:grid-cols-4">
                <OrchStat label="Running" value={orch?.running ?? 0} tone="text-sky-300" />
                <OrchStat label="Queued" value={orch?.queued ?? 0} tone="text-slate-200" />
                <OrchStat label="Blocked" value={orch?.blocked ?? 0} tone={orch && orch.blocked > 0 ? "text-amber-300" : "text-slate-200"} />
                <OrchStat label="Answered today" value={orch?.auto_answered_today ?? 0} tone="text-emerald-300" />
              </div>
              <div className="flex flex-wrap items-center gap-x-3 gap-y-1 text-label text-slate-500">
                <span title="Digest slots run by the scheduler">Digest {digestTimes || "not set"}</span>
                <span title="Studio checks every tool at start and every 3 hours">
                  Studio check {updates.checkedAt ? new Date(updates.checkedAt * 1000).toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" }) : "not yet"}
                </span>
                <span title="The weekly self-build ship">Weekly ship {weeklyShip ? at(weeklyShip) : "not yet"}</span>
                <button onClick={() => go("reports")} className="ml-auto inline-flex items-center gap-1 text-emerald-300 hover:underline">
                  Read today's reports <ArrowRight className="size-3" />
                </button>
              </div>
              {running.length > 0 && (
                <div className="space-y-1.5 border-t border-line pt-2">
                  <div className="text-label font-semibold uppercase tracking-wide text-slate-500">Working now</div>
                  {running.slice(0, 5).map((m) => (
                    <div key={m.id} className="flex flex-wrap items-center gap-2 text-xs">
                      <span className="font-mono text-label text-slate-500">{m.id}</span>
                      <Badge className="bg-sky-500/10 text-sky-300 ring-sky-500/30">{m.agent || "auto"}</Badge>
                      <span className="min-w-0 flex-1 truncate text-slate-200">{m.goal}</span>
                      <span className="text-label text-slate-500">{m.status === "review" ? "in review" : `since ${at(m.created_at)}`}</span>
                      <span className="text-label text-slate-600" title="Last recorded activity">
                        active {at(m.updated_at)}
                      </span>
                    </div>
                  ))}
                  {running.length > 5 && <p className="text-label text-slate-600">{running.length - 5} more running on the Agent Bus board.</p>}
                </div>
              )}
            </div>
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
            <div className="flex items-center justify-between gap-2 border-b border-line px-4 py-3">
              <div className="flex items-center gap-2">
                <Stethoscope className="size-4 text-sky-300" />
                <h2 className="text-sm font-semibold text-slate-100">Fleet doctor</h2>
              </div>
              <Button size="sm" variant="ghost" disabled={busy === "doctor"} onClick={() => void runDoctor()}>
                {busy === "doctor" ? "Checking..." : "Run doctor"}
              </Button>
            </div>
            <div className="space-y-2.5 px-4 py-3">
              <p className="text-label text-slate-400">
                Advisory scan. A blocked mission that stopped on its model or provider is queued again through the scheduler retry budget; everything else is left for you. Hermes may maintain its board database while reading diagnostics.
              </p>
              {!doctor ? (
                <p className="text-label text-slate-500">
                  No doctor pass yet. Runs each automation tick to check kanban diagnostics, 6h stalled missions, and idle repos.
                </p>
              ) : (
                <>
                  {doctor.lastError && (
                    <p role="alert" className="rounded border border-red-500/30 bg-red-500/5 px-2.5 py-2 text-label text-red-300">
                      Check incomplete: {doctor.lastError}
                    </p>
                  )}
                  {!doctor.lastError && doctor.stalledVerdicts.length === 0 &&
                    doctor.diagnosticActions.length === 0 &&
                    doctor.idleRepos.length === 0 && (
                      <p className="text-label text-emerald-300">
                        No findings in the inspected missions and registered repositories.
                      </p>
                    )}
                  {doctor.stalledVerdicts.map((v) => (
                    <div key={v.missionId} className="rounded border border-amber-500/25 bg-amber-500/5 px-2.5 py-2 text-label">
                      <div className="flex items-center justify-between gap-2">
                        <span className="font-medium text-amber-200">
                          {v.missionId} ({v.repo})
                        </span>
                        <span className="text-slate-400">
                          {v.status} · {v.stalledHours}h
                        </span>
                      </div>
                      <p className="mt-1 text-slate-300">Waits on: {v.waitingOn}</p>
                      <p className="text-slate-400">Action: {v.actionTaken} | Next: {v.actionNeeded}</p>
                    </div>
                  ))}
                  {doctor.idleRepos.map((r) => (
                    <div key={r.slug} className="rounded border border-line bg-slate-900/40 px-2.5 py-1.5 text-label">
                      <span className="font-medium text-slate-200">{r.slug}</span>
                      <span className="text-slate-400">: {r.reason}</span>
                    </div>
                  ))}
                  {doctor.diagnosticActions.map((a) => (
                    <div key={`${a.taskId}-${a.kind}`} className="text-label text-slate-300">
                      <span className="font-mono text-sky-300">{a.taskId}</span> ({a.kind}):{" "}
                      <span className={a.action === "retried" ? "text-emerald-300" : a.action === "held" ? "text-amber-300" : "text-slate-400"}>
                        {a.action}
                      </span>{" "}
                      - {a.detail}
                    </div>
                  ))}
                </>
              )}
            </div>
          </Card>

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
            <div className="flex items-center gap-2 border-b border-line px-4 py-3">
              <BookOpenCheck className="size-4 text-violet-300" />
              <h2 className="text-sm font-semibold text-slate-100">Learned in the last day</h2>
            </div>
            <div className="space-y-2 px-4 py-3">
              {learned.length === 0 ? (
                <p className="text-label text-slate-500">
                  Nothing new. Each reviewed mission adds what it taught to the repository's playbook, which every worker
                  there receives.
                </p>
              ) : (
                learned.map((l) => (
                  <div key={l.repo} className="space-y-0.5">
                    <div className="flex items-center gap-2 text-small">
                      <span className="min-w-0 flex-1 truncate text-slate-200">{l.repo}</span>
                      <Badge className="bg-violet-500/10 text-violet-300 ring-violet-500/25">
                        {l.count} lesson{l.count === 1 ? "" : "s"}
                      </Badge>
                    </div>
                    {l.titles.map((t) => (
                      <p key={t} className="truncate text-label text-slate-500" title={t}>
                        {t}
                      </p>
                    ))}
                  </div>
                ))
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
