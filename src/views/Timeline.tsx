import { useEffect, useMemo, useRef, useState } from "react";
import { ArrowLeftRight, ChevronLeft, ChevronRight, Download, GitPullRequest, History, MessageCircleQuestion, Pause, Play, RefreshCw, Users } from "lucide-react";
import { api, errorText, type MissionTimeline, type TimelineSummary } from "../lib/api";
import { Badge, Button, Card, Empty } from "../components/ui";
import { useToast } from "../components/Toasts";

const KIND_STYLE: Record<string, string> = {
  handoff: "bg-indigo-500/10 text-indigo-200 ring-indigo-500/25",
  ask: "bg-sky-500/10 text-sky-200 ring-sky-500/25",
  answer: "bg-sky-500/10 text-sky-200 ring-sky-500/25",
  council: "bg-violet-500/10 text-violet-200 ring-violet-500/25",
  assign: "bg-emerald-500/10 text-emerald-200 ring-emerald-500/25",
  defer: "bg-amber-500/10 text-amber-200 ring-amber-500/25",
  review: "bg-amber-500/10 text-amber-200 ring-amber-500/25",
  denied: "bg-red-500/10 text-red-200 ring-red-500/25",
  pr: "bg-emerald-500/10 text-emerald-200 ring-emerald-500/25",
};

function when(at: number) {
  return at > 0 ? new Date(at * 1000).toLocaleString() : "";
}

function span(s: number) {
  if (s < 60) return `${s}s`;
  if (s < 3600) return `${Math.round(s / 60)} min`;
  return `${(s / 3600).toFixed(1)} h`;
}

/**
 * Run timeline: one mission's path across agents, from creation to merge. Built from the exchange
 * log, the Agent Bus, worker logs, Hermes kanban and state.db (read only) and the memory ledger.
 */
export function Timeline() {
  const toast = useToast();
  const [list, setList] = useState<TimelineSummary[] | null>(null);
  const [picked, setPicked] = useState<string | null>(null);
  const [tl, setTl] = useState<MissionTimeline | null>(null);
  const [cursor, setCursor] = useState<number | null>(null);
  const [playing, setPlaying] = useState(false);
  const [filter, setFilter] = useState<"all" | "exchanges">("all");

  async function loadList() {
    try {
      const l = await api.timelineMissions();
      setList(l);
      if (!picked && l.length) setPicked(l.find((m) => m.exchanges > 0)?.id ?? l[0].id);
    } catch (e) {
      toast("error", errorText(e));
    }
  }

  useEffect(() => {
    void loadList();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // "Why is this here?" in Brain opens the timeline on one mission.
  useEffect(() => {
    const onPick = (e: Event) => {
      const id = (e as CustomEvent<string>).detail;
      if (id) setPicked(id);
    };
    window.addEventListener("agentos:timeline", onPick);
    return () => window.removeEventListener("agentos:timeline", onPick);
  }, []);

  useEffect(() => {
    if (!picked) return;
    setTl(null);
    setCursor(null);
    setPlaying(false);
    api
      .missionTimeline(picked)
      .then(setTl)
      .catch((e) => toast("error", errorText(e)));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [picked]);

  const steps = useMemo(() => {
    const all = tl?.steps ?? [];
    if (filter === "all") return all;
    return all.filter((s) => ["handoff", "ask", "answer", "council", "assign", "defer", "review", "pr", "created", "status"].includes(s.kind));
  }, [tl, filter]);

  // Replay walks the steps in time order, one a second.
  useEffect(() => {
    if (!playing) return;
    const t = window.setInterval(() => {
      setCursor((c) => {
        const next = c === null ? 0 : c + 1;
        if (next >= steps.length) {
          setPlaying(false);
          return steps.length - 1;
        }
        return next;
      });
    }, 1000);
    return () => window.clearInterval(t);
  }, [playing, steps.length]);

  const shown = cursor === null ? steps : steps.slice(0, cursor + 1);

  // Replay and the step buttons keep the current step in view; a long run is taller than the pane.
  const current = useRef<HTMLLIElement | null>(null);
  useEffect(() => {
    if (cursor !== null) current.current?.scrollIntoView({ block: "nearest", behavior: "smooth" });
  }, [cursor]);

  return (
    <div className="flex h-full min-h-0">
      <aside className="w-72 shrink-0 overflow-y-auto border-r border-line/60 p-3">
        <div className="mb-2 flex items-center justify-between">
          <h2 className="flex items-center gap-1.5 text-sm font-semibold text-slate-100">
            <History className="size-4 text-indigo-300" /> Missions
          </h2>
          <button className="text-slate-400 hover:text-slate-200" aria-label="Reload missions" onClick={() => void loadList()}>
            <RefreshCw className="size-3.5" />
          </button>
        </div>
        {list === null ? (
          <p className="text-xs text-slate-500">Loading...</p>
        ) : list.length === 0 ? (
          <p className="text-xs text-slate-500">No missions yet.</p>
        ) : (
          <ul className="flex flex-col gap-1">
            {list.map((m) => (
              <li key={m.id}>
                <button
                  onClick={() => setPicked(m.id)}
                  className={`w-full rounded-lg px-2 py-1.5 text-left text-xs ${picked === m.id ? "bg-white/[0.06] text-slate-100" : "text-slate-300 hover:bg-white/[0.03]"}`}
                >
                  <div className="flex items-center gap-1.5">
                    <span className="font-mono text-slate-500">{m.id}</span>
                    <span className="text-slate-500">{m.status}</span>
                    {m.exchanges > 0 && <span className="ml-auto text-indigo-300">{m.exchanges} exchanges</span>}
                  </div>
                  <div className="truncate" title={m.goal}>
                    {m.goal}
                  </div>
                  <div className="text-slate-500">{m.agents.join(", ")}</div>
                </button>
              </li>
            ))}
          </ul>
        )}
      </aside>

      <section className="min-w-0 flex-1 overflow-y-auto p-6">
        {!picked ? (
          <Empty icon={<History className="size-6" />} title="No mission picked" hint="Queue a mission in Mission Control; its path appears here." />
        ) : !tl ? (
          <p className="text-sm text-slate-500">Building the timeline...</p>
        ) : (
          <>
            <div className="mb-4 flex flex-wrap items-start justify-between gap-3">
              <div className="min-w-0">
                <h1 className="text-lg font-semibold text-slate-100">
                  {tl.mission.id}: {tl.mission.goal}
                </h1>
                <div className="mt-1 flex flex-wrap gap-3 text-xs text-slate-400">
                  <span className="flex items-center gap-1">
                    <Users className="size-3.5" /> {tl.agents.join(", ") || "no agent yet"}
                  </span>
                  <span className="flex items-center gap-1">
                    <ArrowLeftRight className="size-3.5" /> {tl.handoffs} handoffs
                  </span>
                  <span className="flex items-center gap-1">
                    <MessageCircleQuestion className="size-3.5" /> {tl.asks} asks
                  </span>
                  <span>{span(tl.duration_s)}</span>
                  <span>metered ${tl.total_cost_usd.toFixed(2)}</span>
                  <span>{tl.mission.status}</span>
                  {tl.mission.pr_url && (
                    <a className="flex items-center gap-1 text-sky-300 hover:underline" href={tl.mission.pr_url} target="_blank" rel="noreferrer">
                      <GitPullRequest className="size-3.5" /> pull request
                    </a>
                  )}
                </div>
              </div>
              <div className="flex items-center gap-2">
                <select
                  value={filter}
                  onChange={(e) => {
                    setFilter(e.target.value as "all" | "exchanges");
                    setCursor(null);
                  }}
                  aria-label="Which steps to show"
                  className="h-8 rounded-lg bg-white/[0.03] px-2 text-xs text-slate-200 ring-1 ring-line"
                >
                  <option value="all">Every step</option>
                  <option value="exchanges">Exchanges and milestones</option>
                </select>
                <Button size="sm" aria-label="Previous step" onClick={() => setCursor((c) => Math.max(0, (c ?? steps.length) - 1))}>
                  <ChevronLeft className="size-4" />
                </Button>
                <Button
                  size="sm"
                  onClick={() => {
                    if (!playing && (cursor === null || cursor >= steps.length - 1)) setCursor(-1);
                    setPlaying((p) => !p);
                  }}
                >
                  {playing ? <Pause className="size-4" /> : <Play className="size-4" />} {playing ? "Pause" : "Replay"}
                </Button>
                <Button size="sm" aria-label="Next step" onClick={() => setCursor((c) => (c === null ? null : Math.min(steps.length - 1, c + 1)))}>
                  <ChevronRight className="size-4" />
                </Button>
                {cursor !== null && (
                  <Button size="sm" onClick={() => setCursor(null)}>
                    Show all
                  </Button>
                )}
                <Button
                  size="sm"
                  title="Export prompt, tool calls and results to 04_Agent_Engine/runs/ in the vault"
                  onClick={() =>
                    void api
                      .exportMissionRun(tl.mission.id)
                      .then((rel) => toast("ok", `Saved ${rel}`))
                      .catch((e) => toast("error", errorText(e)))
                  }
                >
                  <Download className="size-4" /> Export
                </Button>
              </div>
            </div>
            {cursor !== null && (
              <p className="mb-2 text-xs text-slate-500">
                Step {Math.max(0, cursor + 1)} of {steps.length}
              </p>
            )}
            <ol className="relative flex flex-col gap-2 border-l border-line/60 pl-4">
              {shown.map((s, i) => (
                <li key={`${s.at}-${i}`} ref={cursor !== null && i === cursor ? current : undefined}>
                  <Card className={`p-3 ${cursor !== null && i === cursor ? "ring-1 ring-indigo-400/50" : ""}`}>
                    <div className="flex flex-wrap items-center gap-2 text-xs">
                      <span className="text-slate-500">{when(s.at)}</span>
                      <Badge className={KIND_STYLE[s.kind] ?? "bg-slate-500/10 text-slate-300 ring-slate-500/25"}>{s.kind}</Badge>
                      <span className="font-medium text-slate-200">{s.agent}</span>
                      <span className="text-slate-300">{s.title}</span>
                      {s.duration_ms !== null && <span className="text-slate-500">{(s.duration_ms / 1000).toFixed(1)}s</span>}
                      {s.cost_usd !== null && s.cost_usd > 0 && <span className="text-amber-300">${s.cost_usd.toFixed(3)}</span>}
                      <span className="ml-auto text-slate-600">{s.source}</span>
                    </div>
                    {s.detail && (
                      <details className="mt-1">
                        <summary className="cursor-pointer text-xs text-slate-500">Details</summary>
                        <pre className="mt-1 max-h-64 overflow-auto whitespace-pre-wrap text-xs text-slate-300">{s.detail}</pre>
                      </details>
                    )}
                  </Card>
                </li>
              ))}
            </ol>
          </>
        )}
      </section>
    </div>
  );
}
