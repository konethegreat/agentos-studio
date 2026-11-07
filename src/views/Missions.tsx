import { useEffect, useState } from "react";
import { ChevronDown, ChevronRight, ClipboardCheck, Play, Plus, ShieldCheck } from "lucide-react";
import { api, errorText, type Mission } from "../lib/api";
import { useToast } from "../components/Toasts";

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
  const [form, setForm] = useState({
    repo: repos[0] ?? "",
    goal: "",
    acceptance: "",
    role: "implementer",
    agent: "hermes",
    provider: "claude-subscription",
    budget: "0",
  });

  async function load() {
    try {
      setRows(await api.missions());
    } catch (e) {
      toast("error", errorText(e));
    }
  }

  useEffect(() => {
    void load();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

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
        model: "",
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

  return (
    <section className="border-b border-line bg-[#080c12]">
      <div className="flex items-center gap-2 px-4 py-2">
        <ClipboardCheck className="size-4 text-emerald-300" />
        <span className="text-[12.5px] font-semibold text-slate-200">Missions</span>
        <span className="text-[11px] text-slate-500">
          {rows.length === 0 ? "none yet" : `${rows.filter((r) => r.status !== "done" && r.status !== "failed").length} open`}
        </span>
        <button
          onClick={() => setOpen((v) => !v)}
          className="ml-auto inline-flex h-7 items-center gap-1.5 rounded-lg px-2.5 text-[11.5px] text-slate-300 ring-1 ring-line hover:bg-white/[0.04]"
        >
          {open ? <ChevronDown className="size-3.5" /> : <ChevronRight className="size-3.5" />}
          <Plus className="size-3.5" /> New mission
        </button>
      </div>

      {open && (
        <div className="grid grid-cols-1 gap-2 px-4 pb-3 md:grid-cols-2">
          <label className="text-[11px] text-slate-400">
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
          <label className="text-[11px] text-slate-400">
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
          <label className="text-[11px] text-slate-400 md:col-span-2">
            Goal
            <input
              value={form.goal}
              onChange={(e) => setForm({ ...form, goal: e.target.value })}
              placeholder="Add a missions board to Mission Control"
              className="mt-1 h-8 w-full rounded-lg bg-white/[0.03] px-2 text-xs text-slate-200 outline-none ring-1 ring-line focus:ring-emerald-500/40"
            />
          </label>
          <label className="text-[11px] text-slate-400 md:col-span-2">
            Acceptance criteria (the reviewer judges against these)
            <textarea
              value={form.acceptance}
              onChange={(e) => setForm({ ...form, acceptance: e.target.value })}
              rows={2}
              className="mt-1 w-full rounded-lg bg-white/[0.03] px-2 py-1.5 text-xs text-slate-200 outline-none ring-1 ring-line focus:ring-emerald-500/40"
            />
          </label>
          <label className="text-[11px] text-slate-400">
            Provider
            <select
              value={form.provider}
              onChange={(e) => setForm({ ...form, provider: e.target.value })}
              className="mt-1 h-8 w-full rounded-lg bg-white/[0.03] px-2 text-xs text-slate-200 outline-none ring-1 ring-line"
            >
              <option value="claude-subscription">Claude subscription</option>
              <option value="antigravity">Antigravity subscription</option>
              <option value="openrouter">OpenRouter (metered)</option>
            </select>
          </label>
          <label className="text-[11px] text-slate-400">
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
            <li key={m.id} className="flex items-center gap-2 px-4 py-1.5 text-[11.5px]">
              <span className="font-mono text-[10.5px] text-slate-500">{m.id}</span>
              <span className={`rounded-full px-1.5 py-0.5 text-[10px] font-medium ring-1 ${STATUS_STYLE[m.status] ?? STATUS_STYLE.queued}`}>
                {m.status}
              </span>
              <span className="truncate text-slate-200" title={m.goal}>
                {m.goal}
              </span>
              <span className="hidden shrink-0 text-slate-500 md:inline">{m.repo}</span>
              {m.review_verdict && (
                <span className="shrink-0 text-[10px] text-slate-500">
                  {m.review_verdict} by {m.reviewer}
                </span>
              )}
              <div className="ml-auto flex shrink-0 gap-1">
                {m.status === "queued" && (
                  <button
                    onClick={() => void dispatch(m.id)}
                    disabled={busy}
                    title="Put this mission on the Agent Bus"
                    className="inline-flex h-6 items-center gap-1 rounded-md px-1.5 text-[10.5px] text-sky-300 ring-1 ring-sky-500/25 hover:bg-sky-500/10 disabled:opacity-50"
                  >
                    <Play className="size-3" /> Dispatch
                  </button>
                )}
                {m.status === "review" && (
                  <>
                    <button
                      onClick={() => void judge(m, "approve")}
                      disabled={busy}
                      className="inline-flex h-6 items-center gap-1 rounded-md px-1.5 text-[10.5px] text-emerald-300 ring-1 ring-emerald-500/25 hover:bg-emerald-500/10"
                    >
                      <ShieldCheck className="size-3" /> Approve
                    </button>
                    <button
                      onClick={() => void judge(m, "reject")}
                      disabled={busy}
                      className="inline-flex h-6 items-center gap-1 rounded-md px-1.5 text-[10.5px] text-amber-300 ring-1 ring-amber-500/25 hover:bg-amber-500/10"
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
