import { ArrowRightLeft, Bot, FileText, Play, Terminal } from "lucide-react";
import { useState } from "react";
import { api, errorText, type Task, type TaskState } from "../lib/api";
import { AGENT_COLOR, AGENT_LABEL, PRIORITY_COLOR, relativeTime } from "../lib/format";
import { Badge } from "./ui";
import { useToast } from "./Toasts";

const STATES: { id: TaskState; label: string }[] = [
  { id: "pending", label: "Pending" },
  { id: "in_progress", label: "In Progress" },
  { id: "completed", label: "Completed" },
];

export function TaskCard({ task, running, onOpen }: { task: Task; running: boolean; onOpen: (id: string) => void }) {
  const toast = useToast();
  const [menu, setMenu] = useState(false);
  const [busy, setBusy] = useState<string | null>(null);
  const when = task.completed_at ?? task.claimed_at ?? task.timestamp;

  async function run(agent: "hermes" | "claude-code") {
    setBusy(agent);
    try {
      const w = await api.runNow(task.id, agent);
      toast("ok", `${task.id}: ${AGENT_LABEL[agent]} worker started (pid ${w.pid})`);
      onOpen(task.id);
    } catch (e) {
      toast("error", errorText(e));
    } finally {
      setBusy(null);
    }
  }

  async function move(to: TaskState) {
    setMenu(false);
    setBusy("move");
    try {
      toast("ok", await api.moveTask(task.id, to));
    } catch (e) {
      toast("error", errorText(e));
    } finally {
      setBusy(null);
    }
  }

  const done = task.state === "completed";
  return (
    <div className={`group relative rounded-xl border bg-card p-3.5 transition-colors hover:border-slate-600 ${running ? "border-emerald-500/50 shadow-[0_0_0_1px_rgba(16,185,129,0.15)]" : "border-line"}`}>
      <button onClick={() => onOpen(task.id)} className="block w-full text-left">
        <div className="mb-1.5 flex items-center justify-between gap-2">
          <span className="font-mono text-[11px] text-slate-500">{task.id}</span>
          <span className="text-[11px] text-slate-500">{relativeTime(when)}</span>
        </div>
        <div className="mb-2.5 line-clamp-2 text-sm font-medium leading-snug text-slate-100">{task.title}</div>
        <div className="flex flex-wrap items-center gap-1.5">
          <Badge className="bg-white/5 text-slate-300 ring-white/10">{task.project || "general"}</Badge>
          <Badge className={AGENT_COLOR[task.assigned_to] ?? AGENT_COLOR.human}>
            <Bot className="size-3" />
            {AGENT_LABEL[task.assigned_to] ?? task.assigned_to}
          </Badge>
          <Badge className={PRIORITY_COLOR[task.priority] ?? PRIORITY_COLOR.low}>{task.priority}</Badge>
          {running && (
            <Badge className="bg-emerald-500/15 text-emerald-300 ring-emerald-500/30">
              <span className="size-1.5 animate-pulse rounded-full bg-emerald-400" /> running
            </Badge>
          )}
        </div>
      </button>
      <div className="mt-3 grid grid-cols-2 gap-1.5 border-t border-line pt-3">
        <CardAction icon={<Play className="size-3" />} label="Run Now (Hermes)" busy={busy === "hermes"} disabled={done || running} onClick={() => run("hermes")} />
        <CardAction icon={<Terminal className="size-3" />} label="Run Now (Claude)" busy={busy === "claude-code"} disabled={done || running} onClick={() => run("claude-code")} />
        <div className="relative">
          <CardAction icon={<ArrowRightLeft className="size-3" />} label="Move State" busy={busy === "move"} onClick={() => setMenu((m) => !m)} />
          {menu && (
            <div className="absolute bottom-9 left-0 z-20 w-40 overflow-hidden rounded-lg border border-line bg-raised shadow-xl">
              {STATES.filter((s) => s.id !== task.state).map((s) => (
                <button key={s.id} onClick={() => move(s.id)} className="block w-full px-3 py-2 text-left text-xs text-slate-200 hover:bg-white/5">
                  Move to {s.label}
                </button>
              ))}
            </div>
          )}
        </div>
        <CardAction icon={<FileText className="size-3" />} label="View Spec / Logs" onClick={() => onOpen(task.id)} />
      </div>
    </div>
  );
}

function CardAction({ icon, label, onClick, busy, disabled }: { icon: React.ReactNode; label: string; onClick: () => void; busy?: boolean; disabled?: boolean }) {
  return (
    <button
      onClick={onClick}
      disabled={disabled || busy}
      className="flex h-7 w-full items-center justify-center gap-1.5 rounded-md bg-white/[0.03] px-2 text-[11px] font-medium text-slate-400 ring-1 ring-line transition-colors hover:bg-white/[0.06] hover:text-slate-100 disabled:opacity-40"
    >
      {busy ? <span className="size-3 animate-spin rounded-full border border-slate-400 border-t-transparent" /> : icon}
      <span className="truncate">{label}</span>
    </button>
  );
}
