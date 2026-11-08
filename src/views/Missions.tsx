import { useEffect, useState } from "react";
import { ChevronDown, ChevronRight, ClipboardCheck, GitMerge, Play, Plus, ShieldCheck, Sparkles, Zap } from "lucide-react";
import { invoke } from "@tauri-apps/api/core";
import { api, errorText, type AutomationStatus, type Mission } from "../lib/api";
import { useToast } from "../components/Toasts";
import { Guide, useReportGuides } from "../components/Guides";
import { missionGuides } from "../lib/guides";

const STATUS_STYLE: Record<string, string> = {
  queued: "bg-slate-500/10 text-slate-300 ring-slate-500/25",
  running: "bg-sky-500/10 text-sky-300 ring-sky-500/25",
  review: "bg-amber-500/10 text-amber-300 ring-amber-500/25",
  blocked: "bg-orange-500/10 text-orange-300 ring-orange-500/25",
  done: "bg-emerald-500/10 text-emerald-300 ring-emerald-500/25",
  failed: "bg-red-500/10 text-red-300 ring-red-500/25",
};

const ROLES = ["planner", "implementer", "reviewer", "gatekeeper", "memory"];

/**
 * Missions are the unit that may authorise a merge. A chat never can: a second agent has to
 * approve, and the gate refuses when the reviewer is the implementer.
 */
export function Missions({ repos }: { repos: string[] }) {
  const toast = useToast();
  const [rows, setRows] = useState<Mission[]>([]);
  const [open, setOpen] = useState(false);
  const [busy, setBusy] = useState(false);
  const [auto, setAuto] = useState<AutomationStatus | null>(null);
  const [ideWait, setIdeWait] = useState("");
  const [allowMerge, setAllowMerge] = useState(false);
  const [router, setRouter] = useState<{ role: string; pick: { provider: string; model: string; score: number; reason: string } | null; decline: string | null }[]>([]);
  const [providers, setProviders] = useState<{ id: string; label: string; models: string[]; active: boolean }[]>([]);
  const [form, setForm] = useState({
    repo: repos[0] ?? "",
    goal: "",
    acceptance: "",
    role: "implementer",
    agent: "hermes",
    provider: "claude-subscription",
    model: "",
    budget: "0",
  });

  const [loaded, setLoaded] = useState(false);
  const [pins, setPins] = useState<Record<string, { provider: string; model: string; source: string; reason: string; shadow?: string }>>({});
  const [routerAuto, setRouterAuto] = useState(false);
  const [authors, setAuthors] = useState<Record<string, string>>({});
  async function load() {
    try {
      const list = await api.missions();
      setRows(list);
      void invoke<Record<string, { provider: string; model: string; source: string; reason: string; shadow?: string }>>("mission_pins", { ids: list.map((m) => m.id) })
        .then(setPins)
        .catch(() => undefined);
      const blocked = list.filter((m) => m.status === "blocked").map((m) => m.id);
      if (blocked.length)
        void invoke<Record<string, string>>("mission_notion_authors", { ids: blocked })
          .then(setAuthors)
          .catch(() => undefined);
    } catch (e) {
      toast("error", errorText(e));
    } finally {
      setLoaded(true);
    }
  }

  useEffect(() => {
    void load();
    void api
      .automationStatus()
      .then((a) => {
        setAuto(a);
        setIdeWait(String(a.ide_wait_minutes));
      })
      .catch(() => undefined);
    void api.modelCatalog().then((c) => setProviders(c.providers)).catch(() => undefined);
    void api.routerVerdicts().then(setRouter).catch(() => undefined);
    void invoke<boolean>("router_auto").then(setRouterAuto).catch(() => undefined);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // While automations are on, the scheduler runs in Rust; this keeps the board current.
  useEffect(() => {
    if (!auto?.enabled) return;
    const t = window.setInterval(() => void load(), 12_000);
    return () => window.clearInterval(t);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [auto?.enabled]);

  async function toggleAuto() {
    const next = !(auto?.enabled ?? false);
    try {
      setAuto(await api.setAutomation(next, auto?.max_concurrent));
      toast(next ? "ok" : "info", next ? "Automations on: queued missions start within a minute" : "Automations off: running missions still finish and go to review");
      await load();
    } catch (e) {
      toast("error", errorText(e));
    }
  }

  // Saved on blur, and only when the value really changed, so tabbing through never rewrites it.
  async function saveIdeWait() {
    const n = Math.round(Number(ideWait));
    if (!auto || !Number.isFinite(n) || n === auto.ide_wait_minutes) {
      setIdeWait(auto ? String(auto.ide_wait_minutes) : "");
      return;
    }
    try {
      const next = await api.setAutomation(auto.enabled, auto.max_concurrent, n);
      setAuto(next);
      setIdeWait(String(next.ide_wait_minutes));
      toast("ok", `Qoder and other IDE agents now get ${next.ide_wait_minutes} minutes before a mission is reassigned`);
    } catch (e) {
      toast("error", errorText(e));
    }
  }

  async function tickNow() {
    try {
      const r = await api.automationTick();
      toast("info", `${r.dispatched.length} started, ${r.moved_to_review.length} moved to review`);
      await load();
    } catch (e) {
      toast("error", errorText(e));
    }
  }

  async function create() {
    if (!form.repo.trim() || !form.goal.trim() || !form.acceptance.trim()) {
      toast("error", "Repo, goal and acceptance criteria are required");
      return;
    }
    setBusy(true);
    try {
      await api.createMission({
        repo: form.repo.trim(),
        goal: form.goal.trim(),
        acceptance: form.acceptance.trim(),
        role: form.role,
        agent: form.agent.trim(),
        provider: form.provider,
        model: form.model,
        budgetUsd: Number(form.budget) || 0,
      });
      setForm({ ...form, goal: "", acceptance: "" });
      setOpen(false);
      toast("ok", "Mission queued. Dispatch it to put it on the bus.");
      await load();
    } catch (e) {
      toast("error", errorText(e));
    } finally {
      setBusy(false);
    }
  }

  async function approve(id: string, trust: boolean) {
    setBusy(true);
    try {
      await invoke("approve_mission", { id, trustAuthor: trust });
      toast("ok", trust ? `${id} approved; this Notion author is now trusted` : `${id} is back in the queue`);
      await load();
    } catch (e) {
      toast("error", errorText(e));
    } finally {
      setBusy(false);
    }
  }

  async function dispatch(id: string) {
    setBusy(true);
    try {
      await api.dispatchMission(id);
      toast("ok", "Mission dispatched to the Agent Bus");
      await load();
    } catch (e) {
      toast("error", errorText(e));
    } finally {
      setBusy(false);
    }
  }

  async function agentReview(m: Mission) {
    const reviewer = m.agent === "hermes" ? "claude-code" : "hermes";
    setBusy(true);
    try {
      const r = await api.reviewWithAgent(m.id, reviewer);
      toast("ok", `${reviewer} judged ${r.id}: ${r.review_verdict}`);
      await load();
    } catch (e) {
      toast("error", errorText(e));
    } finally {
      setBusy(false);
    }
  }

  async function merge(m: Mission) {
    setBusy(true);
    try {
      const out = await api.mergeMission(m.id, allowMerge);
      toast("ok", out || `${m.id} merged`);
      await load();
    } catch (e) {
      toast("error", errorText(e));
    } finally {
      setBusy(false);
    }
  }

  async function judge(m: Mission, verdict: "approve" | "reject") {
    const reviewer = m.agent === "hermes" ? "claude-code" : "hermes";
    const note = window.prompt(
      verdict === "approve" ? "Why is this good enough to merge?" : "What must change?",
      verdict === "approve" ? "Acceptance criteria met, tests pass" : "",
    );
    if (!note) return;
    setBusy(true);
    try {
      await api.reviewMission(m.id, reviewer, verdict, note);
      toast(verdict === "approve" ? "ok" : "info", `Mission ${m.id} ${verdict}d`);
      await load();
    } catch (e) {
      toast("error", errorText(e));
    } finally {
      setBusy(false);
    }
  }

  useReportGuides(
    "missions",
    loaded
      ? missionGuides({
          total: rows.length,
          queued: rows.filter((r) => r.status === "queued").length,
          awaitingReview: rows.filter((r) => r.status === "review" && !r.review_verdict).length,
          automationsOn: auto ? auto.enabled : null,
        })
      : [],
  );

  return (
    <section className="border-b border-line bg-sunken">
      <div className="flex items-center gap-2 px-4 py-2">
        <ClipboardCheck className="size-4 text-emerald-300" />
        <span className="text-body-sm font-semibold text-slate-200">Missions</span>
        <span className="text-label text-slate-500">
          {rows.length === 0 ? "none yet" : `${rows.filter((r) => r.status !== "done" && r.status !== "failed").length} open`}
        </span>
        <button
          onClick={() => void toggleAuto()}
          title="Start queued missions automatically, one per repository"
          className={`ml-auto inline-flex h-7 items-center gap-1.5 rounded-lg px-2.5 text-small ring-1 transition-colors ${
            auto?.enabled
              ? "bg-emerald-500/10 text-emerald-300 ring-emerald-500/30"
              : "text-slate-400 ring-line hover:bg-white/[0.04]"
          }`}
        >
          <Zap className="size-3.5" /> Automations {auto?.enabled ? "on" : "off"}
        </button>
        <label
          className="inline-flex h-7 cursor-pointer items-center gap-1.5 rounded-lg px-2 text-label text-slate-400 ring-1 ring-line"
          title="Merging also requires an approving review from another agent, green CI and budget left"
        >
          <input type="checkbox" checked={allowMerge} onChange={(e) => setAllowMerge(e.target.checked)} className="size-3 accent-emerald-500" />
          Allow merge
        </label>
        <button
          onClick={() => void tickNow()}
          title="Run one scheduler pass now"
          className="inline-flex h-7 items-center gap-1.5 rounded-lg px-2.5 text-small text-slate-300 ring-1 ring-line hover:bg-white/[0.04]"
        >
          Run now
        </button>
        <button
          onClick={() => setOpen((v) => !v)}
          className="inline-flex h-7 items-center gap-1.5 rounded-lg px-2.5 text-small text-slate-300 ring-1 ring-line hover:bg-white/[0.04]"
        >
          {open ? <ChevronDown className="size-3.5" /> : <ChevronRight className="size-3.5" />}
          <Plus className="size-3.5" /> New mission
        </button>
      </div>

      <div className="px-4 empty:hidden">
        <Guide id="missions.first" className="mb-2" action={{ label: "New mission", run: () => setOpen(true) }} />
        <Guide id="missions.queued-off" className="mb-2" action={{ label: "Turn Automations on", run: () => void toggleAuto() }} />
        <Guide id="missions.review" className="mb-2" />
      </div>

      <div className="flex flex-wrap items-center gap-x-3 gap-y-1 px-4 pb-2 text-label text-slate-500">
        <label
          className="flex cursor-pointer items-center gap-1.5 text-slate-400"
          title={`Off: the router only shows its pick. On: a new Hermes mission with no model of its own and no .agentos/routing.json entry for its role takes the router's pick, when that pick has at least ${5} decided missions and leads the next by ${5} points. The reason is shown on the mission. Missions already pinned are never changed.`}
        >
          <input
            type="checkbox"
            checked={routerAuto}
            onChange={(e) => {
              const on = e.target.checked;
              void invoke<void>("set_router_auto", { on })
                .then(() => {
                  setRouterAuto(on);
                  toast("ok", on ? "The router picks models for new Hermes missions when its evidence is strong" : "Router back to shadow mode");
                })
                .catch((err) => toast("error", errorText(err)));
            }}
            className="accent-emerald-500"
          />
          Let the router pick
        </label>
        <label
          className="flex items-center gap-1.5 text-slate-400"
          title="Qoder runs only inside its IDE. Its missions are offered to it first; if it has not picked one up after this many minutes, the mission goes to the best headless agent (Claude Code, then Antigravity, then Hermes) with a note."
        >
          IDE pickup wait
          <input
            type="number"
            min={1}
            max={1440}
            value={ideWait}
            onChange={(e) => setIdeWait(e.target.value)}
            onBlur={() => void saveIdeWait()}
            aria-label="Minutes an IDE agent has to pick up a mission"
            className="h-6 w-14 rounded-md border border-line bg-card px-1.5 text-label text-slate-200 outline-none focus:border-emerald-500/50"
          />
          min
        </label>
        {router.length > 0 && (
          <span>
            <span className="text-slate-400">{routerAuto ? "Router:" : "Router (shadow, changes nothing):"}</span>{" "}
            {router.map((v, i) => (
              <span key={v.role}>
                {i > 0 && "; "}
                {v.pick ? (
                  <>
                    {v.role} would use <span className="font-mono text-slate-300">{v.pick.provider}</span>
                    {" / "}
                    <span className="font-mono text-slate-300">{v.pick.model}</span> ({v.pick.reason})
                  </>
                ) : (
                  <>{v.decline}</>
                )}
              </span>
            ))}
          </span>
        )}
      </div>

      {open && (
        <div className="grid grid-cols-1 gap-2 px-4 pb-3 md:grid-cols-2">
          <label className="text-label text-slate-400">
            Repository
            <input
              list="mission-repos"
              value={form.repo}
              onChange={(e) => setForm({ ...form, repo: e.target.value })}
              className="mt-1 h-8 w-full rounded-lg bg-white/[0.03] px-2 text-xs text-slate-200 outline-none ring-1 ring-line focus:ring-emerald-500/40"
            />
            <datalist id="mission-repos">
              {repos.map((r) => (
                <option key={r} value={r} />
              ))}
            </datalist>
          </label>
          <label className="text-label text-slate-400">
            Role and agent
            <div className="mt-1 flex gap-2">
              <select
                value={form.role}
                onChange={(e) => setForm({ ...form, role: e.target.value })}
                className="h-8 flex-1 rounded-lg bg-white/[0.03] px-2 text-xs text-slate-200 outline-none ring-1 ring-line"
              >
                {ROLES.map((r) => (
                  <option key={r} value={r}>
                    {r}
                  </option>
                ))}
              </select>
              <input
                value={form.agent}
                onChange={(e) => setForm({ ...form, agent: e.target.value })}
                title="Agent that does the work"
                className="h-8 w-32 rounded-lg bg-white/[0.03] px-2 text-xs text-slate-200 outline-none ring-1 ring-line"
              />
            </div>
          </label>
          <label className="text-label text-slate-400 md:col-span-2">
            Goal
            <input
              value={form.goal}
              onChange={(e) => setForm({ ...form, goal: e.target.value })}
              placeholder="Add a missions board to Mission Control"
              className="mt-1 h-8 w-full rounded-lg bg-white/[0.03] px-2 text-xs text-slate-200 outline-none ring-1 ring-line focus:ring-emerald-500/40"
            />
          </label>
          <label className="text-label text-slate-400 md:col-span-2">
            Acceptance criteria (the reviewer judges against these)
            <textarea
              value={form.acceptance}
              onChange={(e) => setForm({ ...form, acceptance: e.target.value })}
              rows={2}
              className="mt-1 w-full rounded-lg bg-white/[0.03] px-2 py-1.5 text-xs text-slate-200 outline-none ring-1 ring-line focus:ring-emerald-500/40"
            />
          </label>
          <label className="text-label text-slate-400">
            Provider
            <select
              value={form.provider}
              onChange={(e) => setForm({ ...form, provider: e.target.value, model: "" })}
              className="mt-1 h-8 w-full rounded-lg bg-white/[0.03] px-2 text-xs text-slate-200 outline-none ring-1 ring-line"
            >
              {form.provider && !providers.some((p) => p.id === form.provider) && (
                <option value={form.provider}>{form.provider}</option>
              )}
              {providers.map((p) => (
                <option key={p.id} value={p.id}>
                  {p.label}
                  {p.active ? " (active)" : ""}
                </option>
              ))}
            </select>
          </label>
          <label className="text-label text-slate-400">
            Model (empty means the active model)
            <select
              value={form.model}
              onChange={(e) => setForm({ ...form, model: e.target.value })}
              className="mt-1 h-8 w-full rounded-lg bg-white/[0.03] px-2 text-xs text-slate-200 outline-none ring-1 ring-line"
            >
              <option value="">Inherit active model</option>
              {(providers.find((p) => p.id === form.provider)?.models ?? []).map((m) => (
                <option key={m} value={m}>
                  {m}
                </option>
              ))}
            </select>
          </label>
          <label className="text-label text-slate-400">
            Budget in USD (0 means no OpenRouter spend)
            <input
              value={form.budget}
              onChange={(e) => setForm({ ...form, budget: e.target.value })}
              inputMode="decimal"
              className="mt-1 h-8 w-full rounded-lg bg-white/[0.03] px-2 text-xs text-slate-200 outline-none ring-1 ring-line"
            />
          </label>
          <div className="md:col-span-2">
            <button
              onClick={() => void create()}
              disabled={busy}
              className="inline-flex h-8 items-center gap-1.5 rounded-lg bg-emerald-500/15 px-3 text-xs font-medium text-emerald-300 ring-1 ring-emerald-500/30 hover:bg-emerald-500/20 disabled:opacity-50"
            >
              Queue mission
            </button>
          </div>
        </div>
      )}

      {rows.length > 0 && (
        <ul className="max-h-56 overflow-y-auto border-t border-line/60">
          {rows.map((m) => (
            <li key={m.id} className="flex items-center gap-2 px-4 py-1.5 text-small">
              <span className="font-mono text-caption text-slate-500">{m.id}</span>
              <span className={`rounded-full px-1.5 py-0.5 text-2xs font-medium ring-1 ${STATUS_STYLE[m.status] ?? STATUS_STYLE.queued}`}>
                {m.status}
              </span>
              <span className="truncate text-slate-200" title={m.goal}>
                {m.goal}
              </span>
              <span className="hidden shrink-0 text-slate-500 md:inline">{m.repo}</span>
              {pins[m.id] && (pins[m.id].source !== "inherit" || pins[m.id].reason) && (
                <span className={`shrink-0 text-2xs ${pins[m.id].source === "router" ? "text-sky-300" : "text-slate-500"}`} title={[pins[m.id].reason, pins[m.id].shadow ? `Router: ${pins[m.id].shadow}` : ""].filter(Boolean).join("\n")}>
                  {pins[m.id].source === "inherit" ? "default model" : `${pins[m.id].model || pins[m.id].provider} via ${pins[m.id].source}`}
                </span>
              )}
              {m.pr_url && (
                <a
                  href={m.pr_url}
                  target="_blank"
                  rel="noreferrer"
                  className="shrink-0 text-2xs text-sky-300 hover:underline"
                  onClick={(e) => e.stopPropagation()}
                >
                  PR
                </a>
              )}
              {m.review_verdict && (
                <span className="shrink-0 text-2xs text-slate-500">
                  {m.review_verdict} by {m.reviewer}
                </span>
              )}
              <VerifyBadge json={m.verify} />
              {m.task_id?.startsWith("kanban:") && (
                <span className="shrink-0 font-mono text-2xs text-slate-500" title="Runs on Hermes' kanban board (hermes kanban --board agentos-studio)">
                  {m.task_id.slice(7)}
                </span>
              )}
              {m.spent_usd > 0 && (
                <span className={`shrink-0 text-2xs ${m.budget_usd > 0 && m.spent_usd > m.budget_usd ? "text-rose-300" : "text-slate-500"}`} title="Metered spend from Hermes usage data; subscriptions count zero">
                  ${m.spent_usd.toFixed(2)}
                  {m.budget_usd > 0 ? ` / $${m.budget_usd.toFixed(2)}` : ""}
                </span>
              )}
              <div className="ml-auto flex shrink-0 gap-1">
                {m.status === "blocked" && (
                  <>
                    <button
                      onClick={() => void approve(m.id, false)}
                      disabled={busy}
                      title={m.review_note ?? "Release this mission to the queue"}
                      className="inline-flex h-6 items-center gap-1 rounded-md px-1.5 text-caption text-emerald-300 ring-1 ring-emerald-500/25 hover:bg-emerald-500/10 disabled:opacity-50"
                    >
                      <ShieldCheck className="size-3" /> {authors[m.id] ? "Approve" : "Requeue"}
                    </button>
                    {authors[m.id] && (
                      <button
                        onClick={() => void approve(m.id, true)}
                        disabled={busy}
                        title={`Approve, and queue future proposals from Notion user ${authors[m.id]} without asking`}
                        className="inline-flex h-6 items-center rounded-md px-1.5 text-caption text-slate-300 ring-1 ring-line hover:bg-white/[0.04] disabled:opacity-50"
                      >
                        Approve and trust author
                      </button>
                    )}
                  </>
                )}
                {m.status === "queued" && (
                  <button
                    onClick={() => void dispatch(m.id)}
                    disabled={busy}
                    title={m.agent === "hermes" ? "Put this mission on Hermes' kanban board" : "Put this mission on the Agent Bus"}
                    className="inline-flex h-6 items-center gap-1 rounded-md px-1.5 text-caption text-sky-300 ring-1 ring-sky-500/25 hover:bg-sky-500/10 disabled:opacity-50"
                  >
                    <Play className="size-3" /> Dispatch
                  </button>
                )}
                {m.status === "done" && m.review_verdict === "approve" && m.pr_url && (
                  <button
                    onClick={() => void merge(m)}
                    disabled={busy || !allowMerge}
                    title={allowMerge ? "Merge the approved pull request" : "Tick Allow merge first"}
                    className="inline-flex h-6 items-center gap-1 rounded-md px-1.5 text-caption text-emerald-300 ring-1 ring-emerald-500/25 hover:bg-emerald-500/10 disabled:opacity-40"
                  >
                    <GitMerge className="size-3" /> Merge PR
                  </button>
                )}
                {m.status === "review" && (
                  <>
                    <button
                      onClick={() => void agentReview(m)}
                      disabled={busy}
                      title={`Ask ${m.agent === "hermes" ? "Claude Code" : "Hermes"} to judge the diff`}
                      className="inline-flex h-6 items-center gap-1 rounded-md px-1.5 text-caption text-violet-300 ring-1 ring-violet-500/25 hover:bg-violet-500/10 disabled:opacity-50"
                    >
                      <Sparkles className="size-3" /> Agent review
                    </button>
                    <button
                      onClick={() => void judge(m, "approve")}
                      disabled={busy}
                      className="inline-flex h-6 items-center gap-1 rounded-md px-1.5 text-caption text-emerald-300 ring-1 ring-emerald-500/25 hover:bg-emerald-500/10"
                    >
                      <ShieldCheck className="size-3" /> Approve
                    </button>
                    <button
                      onClick={() => void judge(m, "reject")}
                      disabled={busy}
                      className="inline-flex h-6 items-center gap-1 rounded-md px-1.5 text-caption text-amber-300 ring-1 ring-amber-500/25 hover:bg-amber-500/10"
                    >
                      Reject
                    </button>
                  </>
                )}
              </div>
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}

/** The recorded `hermes verify` result: passed, failed (blocks merge) or not applicable. */
function VerifyBadge({ json }: { json: string | null }) {
  if (!json) return null;
  let v: { ok?: boolean; error?: string; phases?: { phase: string; ok: boolean; command: string }[] } = {};
  try {
    v = JSON.parse(json);
  } catch {
    return null;
  }
  if (v.error === "no-recipe") return <span className="shrink-0 text-2xs text-slate-500" title="hermes verify found no build or test recipe in this repository">verify n/a</span>;
  const failed = v.phases?.find((p) => !p.ok);
  return v.ok ? (
    <span className="shrink-0 text-2xs text-emerald-300" title={(v.phases ?? []).map((p) => `${p.phase}: ${p.command}`).join("\n")}>
      verified
    </span>
  ) : (
    <span className="shrink-0 text-2xs text-rose-300" title={failed ? `${failed.phase}: ${failed.command}` : (v.error ?? "failed")}>
      verify failed
    </span>
  );
}
