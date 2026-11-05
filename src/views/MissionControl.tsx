import { CheckCircle2, CircleDashed, Inbox, LoaderCircle } from "lucide-react";
import type { ReactNode } from "react";
import type { Board, WorkerInfo } from "../lib/api";
import { TaskCard } from "../components/TaskCard";
import { Empty } from "../components/ui";

export function MissionControl({ board, workers, onOpen }: { board: Board | null; workers: WorkerInfo[]; onOpen: (id: string) => void }) {
  const running = new Set(workers.map((w) => w.task_id));
  const columns: { key: keyof Board; title: string; icon: ReactNode; accent: string; empty: string }[] = [
    { key: "pending", title: "Pending", icon: <CircleDashed className="size-4" />, accent: "text-slate-300", empty: "Dispatch a mission or let an agent call delegate_task." },
    { key: "in_progress", title: "In Progress", icon: <LoaderCircle className="size-4" />, accent: "text-sky-300", empty: "Claimed tasks appear here the moment any agent picks one up." },
    { key: "completed", title: "Completed", icon: <CheckCircle2 className="size-4" />, accent: "text-emerald-300", empty: "Finished work with completion summaries." },
  ];
  return (
    <div className="flex h-full min-h-0 flex-col px-6 pb-6 pt-5">
      <div className="mb-4 flex items-end justify-between">
        <div>
          <h1 className="text-lg font-semibold text-slate-100">Live Mission Control</h1>
          <p className="text-xs text-muted">The Agent Bus in your vault, updated live as Hermes, Claude Code, Qoder and Antigravity claim and finish work.</p>
        </div>
        {workers.length > 0 && (
          <span className="inline-flex items-center gap-2 rounded-lg bg-emerald-500/10 px-3 py-1.5 text-xs font-medium text-emerald-300 ring-1 ring-emerald-500/25">
            <span className="size-1.5 animate-pulse rounded-full bg-emerald-400" />
            {workers.length} worker{workers.length > 1 ? "s" : ""} running
          </span>
        )}
      </div>
      <div className="grid min-h-0 flex-1 grid-cols-3 gap-4">
        {columns.map((c) => {
          const tasks = board?.[c.key] ?? [];
          return (
            <section key={c.key} className="flex min-h-0 flex-col rounded-2xl border border-line bg-[#0e141f]">
              <header className="flex items-center justify-between border-b border-line px-4 py-3">
                <div className={`flex items-center gap-2 text-sm font-semibold ${c.accent}`}>
                  {c.icon}
                  {c.title}
                </div>
                <span className="rounded-md bg-white/5 px-2 py-0.5 font-mono text-xs text-slate-400">{tasks.length}</span>
              </header>
              <div className="min-h-0 flex-1 space-y-3 overflow-y-auto p-3">
                {board === null ? (
                  <div className="h-24 animate-pulse rounded-xl bg-white/[0.03]" />
                ) : tasks.length === 0 ? (
                  <Empty icon={<Inbox className="size-5" />} title={`No ${c.title.toLowerCase()} tasks`} hint={c.empty} />
                ) : (
                  tasks.map((t) => <TaskCard key={t.id} task={t} running={running.has(t.id)} onOpen={onOpen} />)
                )}
              </div>
            </section>
          );
        })}
      </div>
    </div>
  );
}
