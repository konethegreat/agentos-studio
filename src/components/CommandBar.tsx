import { CircleCheck, FolderGit2, Rocket, TriangleAlert, Loader2 } from "lucide-react";
import { useState, type FormEvent } from "react";
import { api, errorText, type RepoInfo, type StackStatus } from "../lib/api";
import { useToast } from "./Toasts";

const AGENTS = [
  { id: "auto", label: "Auto / Hermes" },
  { id: "claude-code", label: "Claude Code" },
  { id: "qoder", label: "Qoder" },
  { id: "antigravity", label: "Antigravity" },
];

export function CommandBar({ repos, onDispatched }: { repos: RepoInfo[]; onDispatched: (taskId: string, hasWorker: boolean) => void }) {
  const toast = useToast();
  const [prompt, setPrompt] = useState("");
  const [repo, setRepo] = useState("");
  const [agent, setAgent] = useState("auto");
  const [busy, setBusy] = useState(false);

  async function submit(e: FormEvent) {
    e.preventDefault();
    if (!prompt.trim()) return;
    setBusy(true);
    try {
      const r = await api.dispatch(prompt, repo || null, agent);
      const where = r.worker ? `${r.agent} is working on it now` : `queued for ${r.agent} on the Agent Bus`;
      toast("ok", `${r.task.id} dispatched: ${where}`);
      setPrompt("");
      onDispatched(r.task.id, Boolean(r.worker));
    } catch (err) {
      toast("error", errorText(err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <form onSubmit={submit} className="border-b border-line bg-canvas/90 px-6 pb-3 pt-3.5 backdrop-blur">
      <div className="flex items-center gap-3">
        <div className="flex min-w-0 flex-1 items-center gap-2 rounded-xl border border-line bg-card px-3 focus-within:border-emerald-500/50 focus-within:ring-2 focus-within:ring-emerald-500/10">
          <Rocket className="size-4 shrink-0 text-emerald-400" />
          <input
            value={prompt}
            onChange={(e) => setPrompt(e.target.value)}
            placeholder="Give your agent fleet a mission (e.g., 'Audit ClearCut auth and update Notion docs')..."
            className="h-11 min-w-0 flex-1 bg-transparent text-sm text-slate-100 placeholder:text-slate-500 outline-none"
          />
        </div>
        <button
          type="submit"
          disabled={busy || !prompt.trim()}
          className="inline-flex h-11 shrink-0 items-center gap-2 rounded-xl bg-emerald-500 px-5 text-sm font-semibold text-emerald-950 transition-colors hover:bg-emerald-400 disabled:opacity-50"
        >
          {busy ? <Loader2 className="size-4 animate-spin" /> : <Rocket className="size-4" />}
          Dispatch Mission
        </button>
      </div>
      <div className="mt-2 flex items-center gap-3 pl-1">
        <div className="flex items-center gap-1.5">
          <FolderGit2 className="size-3.5 text-slate-500" />
          <select
            value={repo}
            onChange={(e) => setRepo(e.target.value)}
            className="h-7 max-w-64 cursor-pointer rounded-md border border-line bg-card px-2 text-xs text-slate-300 outline-none hover:border-slate-600"
            aria-label="Repository"
          >
            <option value="">No repository</option>
            {repos.map((r) => (
              <option key={r.path} value={r.path}>
                {r.name}
              </option>
            ))}
          </select>
        </div>
        <span className="h-4 w-px bg-line" />
        <div className="flex items-center gap-1" role="radiogroup" aria-label="Agent">
          {AGENTS.map((a) => (
            <button
              type="button"
              key={a.id}
              role="radio"
              aria-checked={agent === a.id}
              onClick={() => setAgent(a.id)}
              className={`h-7 rounded-md px-2.5 text-xs font-medium transition-colors ${agent === a.id ? "bg-emerald-500/15 text-emerald-300 ring-1 ring-emerald-500/30" : "text-slate-400 hover:bg-white/5 hover:text-slate-200"}`}
            >
              {a.label}
            </button>
          ))}
        </div>
        <span className="ml-auto text-[11px] text-slate-600">
          {agent === "qoder" || agent === "antigravity" ? "Queued on the bus for the IDE agent" : "Runs immediately as a headless worker"}
        </span>
      </div>
    </form>
  );
}

export function HealthPill({ status, onOpenSetup }: { status: StackStatus | null; onOpenSetup: () => void }) {
  if (!status) {
    return (
      <span className="inline-flex h-11 shrink-0 items-center gap-2 rounded-xl border border-line px-3 text-xs text-slate-400">
        <Loader2 className="size-3.5 animate-spin" /> Checking systems
      </span>
    );
  }
  const healthy = status.connected === status.total;
  return (
    <button
      type="button"
      onClick={onOpenSetup}
      className={`inline-flex h-11 shrink-0 items-center gap-2 rounded-xl px-3.5 text-xs font-semibold ring-1 transition-colors ${
        healthy
          ? "bg-emerald-500/10 text-emerald-300 ring-emerald-500/30 hover:bg-emerald-500/15"
          : "bg-amber-500/10 text-amber-300 ring-amber-500/30 hover:bg-amber-500/15"
      }`}
    >
      {healthy ? <CircleCheck className="size-4" /> : <TriangleAlert className="size-4" />}
      {healthy ? `All ${status.total} Systems Connected` : `Setup Required (${status.connected}/${status.total})`}
    </button>
  );
}
