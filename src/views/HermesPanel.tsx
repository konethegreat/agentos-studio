import { BarChart3, Bell, CalendarClock, GitBranch, ShieldCheck, Loader2, Pause, Play, Plus, RefreshCw, Timer, Trash2, Users, Zap } from "lucide-react";
import { useCallback, useEffect, useState } from "react";
import { ApprovalDeclined, useGuarded } from "../components/ApprovalProvider";
import { useToast } from "../components/Toasts";
import { Badge, Button, Card, Field, Modal, inputClass } from "../components/ui";
import { errorText } from "../lib/api";
import { studio, type CronJob, type CronSpec, type HermesProfile } from "../lib/studio";

/** Hermes features shown, not rebuilt: cron, profiles for mission roles, mission notices
 *  through `hermes send`, and `hermes insights`. Every change runs a Hermes command. */
export function HermesPanel() {
  return (
    <div className="space-y-4">
      <CronCard />
      <div className="grid grid-cols-1 gap-4 xl:grid-cols-2">
        <ProfilesCard />
        <NoticesCard />
      </div>
      <div className="grid grid-cols-1 gap-4 xl:grid-cols-2">
        <TimeCapsCard />
        <ParallelCard />
        <TransportCard />
      </div>
      <InsightsCard />
    </div>
  );
}

function Section({ icon, title, hint, actions, children }: { icon: React.ReactNode; title: string; hint: string; actions?: React.ReactNode; children: React.ReactNode }) {
  return (
    <Card className="p-5">
      <div className="mb-3 flex items-start gap-3">
        <div className="grid size-8 shrink-0 place-items-center rounded-lg bg-emerald-500/10 ring-1 ring-emerald-500/25">{icon}</div>
        <div className="min-w-0 flex-1">
          <h2 className="text-sm font-semibold text-slate-100">{title}</h2>
          <p className="text-small text-slate-500">{hint}</p>
        </div>
        {actions}
      </div>
      {children}
    </Card>
  );
}

function CronCard() {
  const toast = useToast();
  const guarded = useGuarded();
  const [jobs, setJobs] = useState<CronJob[] | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [creating, setCreating] = useState(false);
  const load = useCallback(() => {
    void studio.hermes.cronJobs().then(setJobs).catch((e) => toast("error", errorText(e)));
  }, [toast]);
  useEffect(() => {
    load();
  }, [load]);

  async function act(id: string, action: "pause" | "resume" | "run" | "remove") {
    setBusy(id + action);
    try {
      toast("ok", await guarded((a) => studio.hermes.cronAction(id, action, a)));
      load();
    } catch (e) {
      if (!(e instanceof ApprovalDeclined)) toast("error", errorText(e));
    } finally {
      setBusy(null);
    }
  }

  return (
    <Section
      icon={<CalendarClock className="size-4 text-emerald-300" />}
      title="Scheduled jobs (hermes cron)"
      hint="Hermes runs these on its own scheduler, with Studio closed. Studio lists them and changes them only through hermes cron."
      actions={
        <div className="flex gap-2">
          <Button size="sm" icon={<RefreshCw className="size-3.5" />} onClick={load}>
            Refresh
          </Button>
          <Button size="sm" variant="primary" icon={<Plus className="size-3.5" />} onClick={() => setCreating(true)}>
            New job
          </Button>
        </div>
      }
    >
      {!jobs && <Loader2 className="mx-auto size-5 animate-spin text-slate-500" />}
      {jobs?.length === 0 && <p className="text-small text-slate-500">No scheduled jobs. A job is a prompt Hermes runs on a schedule, such as "every 2h" or "0 9 * * 1".</p>}
      {jobs && jobs.length > 0 && (
        <div className="divide-y divide-line rounded-lg ring-1 ring-line">
          {jobs.map((j) => (
            <div key={j.id} className="flex items-center gap-3 px-3 py-2.5">
              <div className="min-w-0 flex-1">
                <div className="flex items-center gap-2 text-sm text-slate-200">
                  {j.name || j.id}
                  <Badge className={j.state === "paused" ? "bg-amber-500/10 text-amber-300 ring-amber-500/25" : "bg-emerald-500/10 text-emerald-300 ring-emerald-500/25"}>{j.state ?? (j.enabled ? "active" : "off")}</Badge>
                  <span className="font-mono text-label text-slate-500">{j.schedule_display}</span>
                </div>
                <div className="truncate text-small text-slate-500" title={j.prompt ?? ""}>
                  {j.prompt}
                </div>
                <div className="text-label text-slate-600">
                  {j.next_run_at ? `next ${new Date(j.next_run_at).toLocaleString()}` : "not scheduled"}
                  {j.last_status ? ` · last ${j.last_status}` : ""}
                  {j.last_error ? ` · ${j.last_error}` : ""}
                </div>
              </div>
              {j.state === "paused" ? (
                <Button size="sm" busy={busy === j.id + "resume"} icon={<Play className="size-3.5" />} onClick={() => void act(j.id, "resume")}>
                  Resume
                </Button>
              ) : (
                <Button size="sm" busy={busy === j.id + "pause"} icon={<Pause className="size-3.5" />} onClick={() => void act(j.id, "pause")}>
                  Pause
                </Button>
              )}
              <Button size="sm" variant="ghost" busy={busy === j.id + "run"} icon={<Zap className="size-3.5" />} onClick={() => void act(j.id, "run")}>
                Run now
              </Button>
              <Button size="sm" variant="ghost" busy={busy === j.id + "remove"} icon={<Trash2 className="size-3.5" />} onClick={() => void act(j.id, "remove")} aria-label={`Remove ${j.name || j.id}`} />
            </div>
          ))}
        </div>
      )}
      {creating && <CronCreate onClose={() => setCreating(false)} onCreated={load} />}
    </Section>
  );
}

function CronCreate({ onClose, onCreated }: { onClose: () => void; onCreated: () => void }) {
  const toast = useToast();
  const guarded = useGuarded();
  const [spec, setSpec] = useState<CronSpec>({ schedule: "every 1d", prompt: "", name: "", workdir: "", paused: false });
  const [busy, setBusy] = useState(false);
  async function create() {
    setBusy(true);
    try {
      toast("ok", await guarded((a) => studio.hermes.cronCreate(spec, a)));
      onCreated();
      onClose();
    } catch (e) {
      if (!(e instanceof ApprovalDeclined)) toast("error", errorText(e));
    } finally {
      setBusy(false);
    }
  }
  return (
    <Modal
      title="New Hermes cron job"
      onClose={onClose}
      footer={
        <Button variant="primary" busy={busy} disabled={!spec.schedule.trim() || !spec.prompt.trim()} onClick={() => void create()}>
          Create with hermes cron
        </Button>
      }
    >
      <div className="space-y-3">
        <Field label="Schedule" hint={'"30m", "every 2h" or a cron expression such as "0 9 * * 1"'}>
          <input className={inputClass} value={spec.schedule} onChange={(e) => setSpec({ ...spec, schedule: e.target.value })} />
        </Field>
        <Field label="Prompt" hint="A self-contained task: the job starts a fresh session each time.">
          <textarea rows={4} className={`${inputClass} resize-y`} value={spec.prompt} onChange={(e) => setSpec({ ...spec, prompt: e.target.value })} />
        </Field>
        <Field label="Name (optional)">
          <input className={inputClass} value={spec.name} onChange={(e) => setSpec({ ...spec, name: e.target.value })} />
        </Field>
        <Field label="Working folder (optional)">
          <input className={`${inputClass} font-mono text-xs`} value={spec.workdir} onChange={(e) => setSpec({ ...spec, workdir: e.target.value })} placeholder="C:/Users/you/repo" />
        </Field>
        <label className="flex items-center gap-2 text-small text-slate-300">
          <input type="checkbox" checked={!!spec.paused} onChange={(e) => setSpec({ ...spec, paused: e.target.checked })} className="accent-emerald-500" /> Create paused
        </label>
      </div>
    </Modal>
  );
}

function ProfilesCard() {
  const toast = useToast();
  const [profiles, setProfiles] = useState<HermesProfile[] | null>(null);
  const load = useCallback(() => {
    void studio.hermes.profiles().then(setProfiles).catch((e) => toast("error", errorText(e)));
  }, [toast]);
  useEffect(() => {
    load();
  }, [load]);
  const current = (role: string) => profiles?.find((p) => p.roles.includes(role))?.name ?? "";
  async function assign(role: "implementer" | "reviewer", name: string) {
    try {
      await studio.hermes.setRoleProfile(role, name);
      load();
    } catch (e) {
      toast("error", errorText(e));
    }
  }
  return (
    <Section
      icon={<Users className="size-4 text-emerald-300" />}
      title="Hermes profiles for missions"
      hint="Pick which Hermes profile implements and which reviews Hermes missions. Create profiles with hermes profile create; a reviewer on another profile keeps its own memory and model."
    >
      {!profiles && <Loader2 className="mx-auto size-5 animate-spin text-slate-500" />}
      {profiles && (
        <>
          <ul className="mb-3 space-y-1 text-small">
            {profiles.map((p) => (
              <li key={p.name} className="flex items-center gap-2">
                <span className="font-medium text-slate-200">{p.name}</span>
                <span className="font-mono text-label text-slate-500">{p.model || "no model set"}</span>
                {p.roles.map((r) => (
                  <Badge key={r} className="bg-white/5 text-slate-300 ring-white/10">
                    {r}
                  </Badge>
                ))}
              </li>
            ))}
          </ul>
          <div className="grid grid-cols-2 gap-3">
            {(["implementer", "reviewer"] as const).map((role) => (
              <Field key={role} label={role === "implementer" ? "Implementer profile" : "Reviewer profile"}>
                <select className={inputClass} value={current(role)} onChange={(e) => void assign(role, e.target.value)}>
                  <option value="">Not set (default)</option>
                  {profiles.map((p) => (
                    <option key={p.name} value={p.name}>
                      {p.name}
                    </option>
                  ))}
                </select>
              </Field>
            ))}
          </div>
        </>
      )}
    </Section>
  );
}

function NoticesCard() {
  const toast = useToast();
  const [target, setTarget] = useState("");
  const [saved, setSaved] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  useEffect(() => {
    void studio.hermes
      .notifyTarget()
      .then((t) => {
        setSaved(t);
        setTarget(t ?? "");
      })
      .catch(() => undefined);
  }, []);
  async function save(test: boolean) {
    setBusy(true);
    try {
      toast("ok", await studio.hermes.setNotifyTarget(target, test));
      setSaved(target.trim() || null);
    } catch (e) {
      toast("error", errorText(e));
    } finally {
      setBusy(false);
    }
  }
  return (
    <Section
      icon={<Bell className="size-4 text-emerald-300" />}
      title="Mission notices (hermes send)"
      hint="When a mission is done or fails, Hermes sends one line to a messaging platform it is already set up for. Off when empty."
    >
      <Field label="Target" hint={'A platform ("whatsapp", "telegram") or platform:chat_id. hermes send -l lists them.'}>
        <input className={`${inputClass} font-mono text-xs`} value={target} onChange={(e) => setTarget(e.target.value)} placeholder="telegram" />
      </Field>
      <div className="mt-3 flex items-center gap-2">
        <Button size="sm" variant="primary" busy={busy} disabled={!target.trim()} onClick={() => void save(true)}>
          Save and send a test
        </Button>
        <Button size="sm" busy={busy} onClick={() => void save(false)}>
          {target.trim() ? "Save" : "Turn off"}
        </Button>
        <span className="text-label text-slate-500">{saved ? `Now: ${saved}` : "Now: off"}</span>
      </div>
    </Section>
  );
}

function InsightsCard() {
  const toast = useToast();
  const [days, setDays] = useState(7);
  const [text, setText] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  async function run() {
    setBusy(true);
    try {
      setText(await studio.hermes.insights(days));
    } catch (e) {
      toast("error", errorText(e));
    } finally {
      setBusy(false);
    }
  }
  return (
    <Section
      icon={<BarChart3 className="size-4 text-emerald-300" />}
      title="Insights (hermes insights)"
      hint="Hermes' own report over its session history: sessions, tokens, cost, models and tool use."
      actions={
        <div className="flex items-center gap-2">
          <select className="h-8 rounded-md border border-line bg-card px-2 text-xs text-slate-200" value={days} onChange={(e) => setDays(Number(e.target.value))} aria-label="Period">
            {[1, 7, 30, 90].map((d) => (
              <option key={d} value={d}>
                Last {d} day{d > 1 ? "s" : ""}
              </option>
            ))}
          </select>
          <Button size="sm" variant="primary" busy={busy} onClick={() => void run()}>
            {text ? "Run again" : "Run"}
          </Button>
        </div>
      }
    >
      {text ? <pre className="max-h-[28rem] overflow-auto rounded-lg bg-inset p-3 font-mono text-label leading-relaxed text-slate-300">{text}</pre> : <p className="text-small text-slate-500">Runs on demand; it reads the whole session history, so it can take a few seconds.</p>}
    </Section>
  );
}

function TimeCapsCard() {
  const toast = useToast();
  const [mission, setMission] = useState("");
  const [run, setRun] = useState("");
  const [busy, setBusy] = useState(false);
  useEffect(() => {
    void studio.hermes
      .timeCaps()
      .then((c) => {
        setMission(c.mission_max_runtime ? String(Math.round(c.mission_max_runtime / 60)) : "");
        setRun(c.run_budget_seconds ? String(Math.round(c.run_budget_seconds / 60)) : "");
      })
      .catch(() => undefined);
  }, []);
  const secs = (m: string) => {
    const n = Number(m);
    return m.trim() && n > 0 ? Math.round(n * 60) : null;
  };
  async function save() {
    setBusy(true);
    try {
      await studio.hermes.setTimeCaps(secs(mission), secs(run));
      toast("ok", "Time caps saved");
    } catch (e) {
      toast("error", errorText(e));
    } finally {
      setBusy(false);
    }
  }
  return (
    <Section
      icon={<Timer className="size-4 text-emerald-300" />}
      title="Time caps"
      hint="Budgets in dollars come from Hermes usage data and count only metered providers. Time caps stop runaway runs on any provider."
    >
      <div className="grid grid-cols-1 gap-3 md:grid-cols-2">
        <Field label="Hermes mission cap (minutes)" hint="Passed to hermes kanban create --max-runtime for each Hermes mission. Empty: no cap.">
          <input className={inputClass} inputMode="numeric" value={mission} onChange={(e) => setMission(e.target.value.replace(/[^0-9]/g, ""))} placeholder="60" />
        </Field>
        <Field label="Every Hermes run (minutes)" hint="agent.run_budget_seconds in Hermes' config, set with hermes config set. Applies to chats too. Empty: Hermes default.">
          <input className={inputClass} inputMode="numeric" value={run} onChange={(e) => setRun(e.target.value.replace(/[^0-9]/g, ""))} placeholder="none" />
        </Field>
      </div>
      <div className="mt-3">
        <Button size="sm" variant="primary" busy={busy} onClick={() => void save()}>
          Save
        </Button>
      </div>
    </Section>
  );
}

function ParallelCard() {
  const toast = useToast();
  const [n, setN] = useState<number | null>(null);
  useEffect(() => {
    void studio.hermes.parallelPerRepo().then(setN).catch(() => setN(1));
  }, []);
  async function save(v: number) {
    try {
      await studio.hermes.setParallelPerRepo(v);
      setN(v);
      toast("ok", v > 1 ? `Up to ${v} Hermes missions per repository, each in its own worktree` : "One mission per repository");
    } catch (e) {
      toast("error", errorText(e));
    }
  }
  return (
    <Section
      icon={<GitBranch className="size-4 text-emerald-300" />}
      title="Parallel missions in one repository"
      hint="Above 1, each Hermes mission runs in its own git worktree (<repo>/.worktrees/<task>, its own branch), so several work on one repository at once. Claude Code and Antigravity missions share the checkout, so they always run alone in their repository. The global cap in Mission Control still applies."
    >
      <Field label="Hermes missions per repository">
        <select className={inputClass} value={n ?? 1} disabled={n === null} onChange={(e) => void save(Number(e.target.value))}>
          {[1, 2, 3, 4].map((v) => (
            <option key={v} value={v}>
              {v === 1 ? "1 (no worktrees)" : `${v}, on worktrees`}
            </option>
          ))}
        </select>
      </Field>
    </Section>
  );
}

function TransportCard() {
  const toast = useToast();
  const [t, setT] = useState<"acp" | "cli" | null>(null);
  const [mins, setMins] = useState<string>("");
  useEffect(() => {
    void studio.chat.transport().then(setT).catch(() => setT("acp"));
    void studio.chat
      .approvalTimeout()
      .then((s) => setMins(String(Math.round((s / 60) * 10) / 10)))
      .catch(() => setMins("10"));
  }, []);
  async function saveTimeout() {
    const m = Number(mins);
    if (!Number.isFinite(m) || m <= 0) return toast("error", "Enter minutes, for example 10");
    try {
      const s = await studio.chat.setApprovalTimeout(Math.round(m * 60));
      setMins(String(Math.round((s / 60) * 10) / 10));
      toast("ok", `Unanswered approvals are denied after ${Math.round((s / 60) * 10) / 10} min`);
    } catch (e) {
      toast("error", errorText(e));
    }
  }
  async function save(acp: boolean) {
    try {
      await studio.chat.setTransport(acp);
      setT(acp ? "acp" : "cli");
      toast("ok", acp ? "Hermes chat runs over ACP" : "Hermes chat runs as hermes chat -q");
    } catch (e) {
      toast("error", errorText(e));
    }
  }
  return (
    <Section
      icon={<ShieldCheck className="size-4 text-emerald-300" />}
      title="Chat transport"
      hint="ACP (recommended): Hermes asks before dangerous commands, Stop cancels the turn cleanly, and reasoning streams into the Thinking panel. The chat is created with its model, skills and checkpoints, then every turn runs on one long-lived hermes acp connection. If ACP fails before a message is sent, that turn uses hermes chat -q instead. hermes chat -q: every dangerous command is denied (approvals.single_query_mode) and Stop ends the process."
    >
      <Field label="Hermes chat runs over">
        <select className={inputClass} value={t ?? "acp"} disabled={t === null} onChange={(e) => void save(e.target.value === "acp")}>
          <option value="acp">ACP: approvals, cancel, reasoning</option>
          <option value="cli">hermes chat -q</option>
        </select>
      </Field>
      <Field label="Deny an unanswered approval after (minutes)">
        <input className={inputClass} inputMode="decimal" value={mins} onChange={(e) => setMins(e.target.value)} onBlur={() => void saveTimeout()} onKeyDown={(e) => e.key === "Enter" && void saveTimeout()} aria-label="Approval timeout in minutes" />
      </Field>
    </Section>
  );
}
