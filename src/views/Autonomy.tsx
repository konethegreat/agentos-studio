import { useCallback, useEffect, useState } from "react";
import { Bot, CirclePause, CirclePlay, ListChecks, Play, RefreshCw, ShieldCheck } from "lucide-react";
import { api, errorText, type AutonomyRepo, type AutonomySettings } from "../lib/api";
import { Badge, Button, Card, Empty, Toggle, inputClass } from "../components/ui";
import { useToast } from "../components/Toasts";

function when(at: number): string {
  return at ? new Date(at * 1000).toLocaleString() : "";
}

function progress(r: AutonomyRepo): { done: number; total: number } {
  const done = r.phases.reduce((n, p) => n + p.done, 0);
  return { done, total: done + r.phases.reduce((n, p) => n + p.open.length, 0) };
}

/**
 * Repositories that keep themselves busy (docs/AUTONOMOUS-REPOS.md): each opted-in repository
 * carries docs/agentos/PHASES.md and TASKS.md, and Studio keeps one goal in flight for it.
 */
export function Autonomy({ onOpenGoals }: { onOpenGoals?: () => void }) {
  const toast = useToast();
  const [repos, setRepos] = useState<AutonomyRepo[] | null>(null);
  const [settings, setSettings] = useState<AutonomySettings | null>(null);
  const [busy, setBusy] = useState("");
  const [level, setLevel] = useState("full");
  const [filter, setFilter] = useState<"all" | "in" | "out">("all");

  const load = useCallback(async () => {
    // Settings answer at once; the repository list walks the scan roots and takes seconds.
    try {
      setSettings(await api.autonomySettings());
      setRepos(await api.autonomyStatus());
    } catch (e) {
      toast("error", errorText(e));
    }
  }, [toast]);

  useEffect(() => {
    void load();
  }, [load]);

  async function act(label: string, f: () => Promise<unknown>, ok?: (v: unknown) => string) {
    setBusy(label);
    try {
      const v = await f();
      if (ok) toast("ok", ok(v));
      await load();
    } catch (e) {
      toast("error", errorText(e));
    } finally {
      setBusy("");
    }
  }

  const shown = (repos ?? []).filter((r) => (filter === "all" ? true : filter === "in" ? r.enabled : !r.enabled));
  const optedIn = (repos ?? []).filter((r) => r.enabled).length;

  return (
    <div className="space-y-4 p-5">
      <Card className="space-y-3 p-4">
        <div className="flex flex-wrap items-center justify-between gap-3">
          <div>
            <div className="flex items-center gap-2 text-base font-semibold text-slate-100">
              <Bot className="size-5" /> Repositories that keep themselves busy
            </div>
            <p className="mt-1 max-w-3xl text-small text-muted">
              Each opted-in repository keeps its own plan in docs/agentos/PHASES.md and TASKS.md. Studio keeps one goal in
              flight for it: write the plan, finish the next phase task by task, or write the next phase. Every task is a
              pull request that the reviewer judges and the gate merges, and the agent ticks the task and adds a line to
              docs/agentos/REPORT.md in the same pull request.
            </p>
          </div>
          {settings && (
            <div className="flex items-center gap-4">
              <Toggle
                checked={settings.enabled}
                label={settings.enabled ? "On" : "Off"}
                onChange={(on) => void act("switch", () => api.autonomySet(on, settings.max_active))}
              />
              <label className="flex items-center gap-2 whitespace-nowrap text-small text-slate-300">
                At once
                <select
                  className={`${inputClass} !w-20`}
                  value={settings.max_active}
                  onChange={(e) => void act("max", () => api.autonomySet(settings.enabled, Number(e.target.value)))}
                >
                  {[1, 2, 3, 4, 5, 6, 8, 10, 12].map((n) => (
                    <option key={n} value={n}>
                      {n}
                    </option>
                  ))}
                </select>
              </label>
              <Button
                size="sm"
                busy={busy === "run"}
                disabled={!settings.enabled}
                icon={<Play className="size-3.5" />}
                onClick={() =>
                  void act("run", () => api.autonomyRunNow(), (v) => {
                    const r = v as { considered: number; started: string[] };
                    return r.started.length ? `Started ${r.started.join(", ")}.` : `Checked ${r.considered} repositories; nothing to start.`;
                  })
                }
              >
                Run a pass now
              </Button>
            </div>
          )}
        </div>
        {settings && settings.enabled && !settings.automation_on && (
          <div className="rounded-lg bg-amber-500/[0.06] px-3 py-2 text-small text-amber-200 ring-1 ring-amber-500/25">
            Automations are off, so goals are planned but their missions do not start on their own. Turn Automations on in
            Mission Control.
          </div>
        )}
        {settings?.paused && (
          <div className="rounded-lg bg-amber-500/[0.06] px-3 py-2 text-small text-amber-200 ring-1 ring-amber-500/25">
            The fleet is paused; nothing starts until you resume it.
          </div>
        )}
        <div className="flex flex-wrap items-center gap-3 text-small text-slate-300">
          <span>
            {optedIn} of {repos?.length ?? 0} repositories opted in.
          </span>
          <label className="flex items-center gap-2 whitespace-nowrap">
            Opt in at
            <select className={`${inputClass} !w-56`} value={level} onChange={(e) => setLevel(e.target.value)}>
              <option value="full">Full: the gate merges</option>
              <option value="ask">Ask on risk</option>
              <option value="watch">Watch: you click each step</option>
            </select>
          </label>
          <select className={`${inputClass} !w-48`} value={filter} onChange={(e) => setFilter(e.target.value as typeof filter)}>
            <option value="all">Every repository</option>
            <option value="in">Opted in</option>
            <option value="out">Not opted in</option>
          </select>
          <Button size="sm" variant="ghost" icon={<RefreshCw className="size-3.5" />} onClick={() => void load()}>
            Refresh
          </Button>
        </div>
      </Card>

      {repos === null ? (
        <div className="text-small text-muted">Reading the repositories...</div>
      ) : shown.length === 0 ? (
        <Empty icon={<Bot className="size-5" />} title="No repositories here" hint="Studio lists every git repository under the scan roots in Settings, plus your Hermes projects." />
      ) : (
        <div className="grid gap-3 xl:grid-cols-2">
          {shown.map((r) => {
            const p = progress(r);
            return (
              <Card key={r.slug} className="space-y-2 p-4">
                <div className="flex items-start justify-between gap-2">
                  <div className="min-w-0">
                    <div className="flex items-center gap-2 text-small font-semibold text-slate-100">
                      {r.slug}
                      {r.enabled && <Badge>{r.paused_here ? "paused here" : r.level}</Badge>}
                      {r.enabled && r.allow_merge && (
                        <span title="allow_merge is on in routing.json">
                          <ShieldCheck className="size-4 text-emerald-300" />
                        </span>
                      )}
                    </div>
                    <div className="truncate text-2xs text-muted">
                      {r.path}
                      {!r.hermes_project && " . not a Hermes project yet; Opt in makes it one"}
                    </div>
                  </div>
                  <div className="flex shrink-0 gap-1.5">
                    {!r.enabled ? (
                      <Button
                        size="sm"
                        variant="primary"
                        busy={busy === `opt:${r.slug}`}
                        onClick={() =>
                          void act(`opt:${r.slug}`, () => api.autonomyOptIn(r.slug, r.path, level), (u) => `Opted in: ${String(u)}`)
                        }
                      >
                        Opt in
                      </Button>
                    ) : (
                      <Button
                        size="sm"
                        variant="ghost"
                        icon={r.paused_here ? <CirclePlay className="size-3.5" /> : <CirclePause className="size-3.5" />}
                        onClick={() => void act(`pause:${r.slug}`, () => api.autonomyPauseRepo(r.slug, !r.paused_here))}
                      >
                        {r.paused_here ? "Resume" : "Pause here"}
                      </Button>
                    )}
                  </div>
                </div>
                {r.enabled && (
                  <>
                    <div className="text-small text-slate-300">
                      {r.goal ? (
                        <button className="underline" onClick={onOpenGoals}>
                          {r.goal}
                        </button>
                      ) : null}
                      {r.goal ? " . " : ""}
                      {r.next}
                    </div>
                    {r.has_plan ? (
                      <div className="space-y-1">
                        <div className="h-1.5 overflow-hidden rounded-full bg-white/[0.06]">
                          <div className="h-full bg-emerald-400/70" style={{ width: `${p.total ? (100 * p.done) / p.total : 0}%` }} />
                        </div>
                        <div className="flex items-center gap-1.5 text-2xs text-muted">
                          <ListChecks className="size-3.5" /> {p.done} of {p.total} tasks done across {r.phases.length} phases
                          {r.phases.find((x) => x.open.length) && `; now: ${r.phases.find((x) => x.open.length)?.title}`}
                        </div>
                      </div>
                    ) : (
                      <div className="text-2xs text-muted">No plan on the default branch yet; the first goal writes it.</div>
                    )}
                    {r.log.length > 0 && (
                      <ul className="space-y-0.5 border-t border-line pt-2">
                        {r.log.slice(0, 4).map((e, i) => (
                          <li key={i} className="truncate text-2xs text-slate-400">
                            {when(e.at)} {e.kind}
                            {e.goal ? ` ${e.goal}` : ""}: {e.text}
                          </li>
                        ))}
                      </ul>
                    )}
                  </>
                )}
              </Card>
            );
          })}
        </div>
      )}
    </div>
  );
}
