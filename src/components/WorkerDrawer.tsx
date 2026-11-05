import { Square, X, FileText, Terminal } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { api, errorText, type Task, type WorkerEvent, type WorkerInfo } from "../lib/api";
import { AGENT_LABEL, relativeTime } from "../lib/format";
import { Button } from "./ui";
import { useToast } from "./Toasts";

export function WorkerDrawer({
  taskId,
  worker,
  liveLines,
  onClose,
}: {
  taskId: string;
  worker: WorkerInfo | undefined;
  liveLines: WorkerEvent[];
  onClose: () => void;
}) {
  const toast = useToast();
  const [tab, setTab] = useState<"stream" | "spec">(worker ? "stream" : "spec");
  const [task, setTask] = useState<Task | null>(null);
  const [markdown, setMarkdown] = useState("");
  const [log, setLog] = useState("");
  const [stopping, setStopping] = useState(false);
  const bottom = useRef<HTMLDivElement>(null);

  useEffect(() => {
    let alive = true;
    api
      .readTask(taskId)
      .then((r) => {
        if (!alive) return;
        setTask(r.task);
        setMarkdown(r.markdown);
        setLog(r.log);
        if (!worker && r.log) setTab("stream");
      })
      .catch((e) => toast("error", errorText(e)));
    return () => {
      alive = false;
    };
  }, [taskId, worker, toast]);

  useEffect(() => {
    bottom.current?.scrollIntoView({ block: "end" });
  }, [liveLines.length, tab]);

  async function stop() {
    setStopping(true);
    try {
      toast("ok", await api.stopWorker(taskId));
    } catch (e) {
      toast("error", errorText(e));
    } finally {
      setStopping(false);
    }
  }

  const color = (s: WorkerEvent["stream"]) =>
    s === "stderr" ? "text-amber-300/90" : s === "system" ? "text-sky-300" : s === "exit" ? "text-emerald-300" : "text-slate-200";

  return (
    <div className="fixed inset-0 z-40 flex justify-end bg-black/40" onMouseDown={onClose}>
      <aside className="animate-slide-in flex h-full w-[min(760px,92vw)] flex-col border-l border-line bg-[#0d121c] shadow-2xl" onMouseDown={(e) => e.stopPropagation()}>
        <header className="flex items-start justify-between gap-4 border-b border-line px-5 py-4">
          <div className="min-w-0">
            <div className="font-mono text-xs text-slate-500">{taskId}</div>
            <h2 className="truncate text-base font-semibold text-slate-100">{task?.title ?? "Loading task"}</h2>
            <div className="mt-1 text-xs text-slate-500">
              {task && `${task.state.replace("_", " ")} - ${AGENT_LABEL[task.assigned_to] ?? task.assigned_to} - created ${relativeTime(task.timestamp)}`}
            </div>
          </div>
          <div className="flex shrink-0 items-center gap-2">
            {worker && (
              <Button variant="danger" size="sm" busy={stopping} icon={<Square className="size-3" />} onClick={stop}>
                Stop Worker
              </Button>
            )}
            <button onClick={onClose} className="rounded-md p-1.5 text-slate-500 hover:bg-white/5 hover:text-slate-200" aria-label="Close drawer">
              <X className="size-4" />
            </button>
          </div>
        </header>
        <div className="flex gap-1 border-b border-line px-5 pt-2">
          {(["stream", "spec"] as const).map((t) => (
            <button
              key={t}
              onClick={() => setTab(t)}
              className={`-mb-px flex items-center gap-1.5 border-b-2 px-3 py-2 text-xs font-medium ${tab === t ? "border-emerald-400 text-slate-100" : "border-transparent text-slate-500 hover:text-slate-300"}`}
            >
              {t === "stream" ? <Terminal className="size-3.5" /> : <FileText className="size-3.5" />}
              {t === "stream" ? (worker ? "Live worker" : "Worker log") : "Full spec"}
              {t === "stream" && worker && <span className="size-1.5 animate-pulse rounded-full bg-emerald-400" />}
            </button>
          ))}
        </div>
        <div className="min-h-0 flex-1 overflow-auto px-5 py-4 font-mono text-[12px] leading-relaxed">
          {tab === "spec" ? (
            <pre className="whitespace-pre-wrap break-words text-slate-300">{markdown}</pre>
          ) : (
            <>
              {liveLines.length === 0 && log && <pre className="whitespace-pre-wrap break-words text-slate-400">{log}</pre>}
              {liveLines.map((l, i) => (
                <div key={i} className={`whitespace-pre-wrap break-words ${color(l.stream)}`}>
                  {l.line}
                </div>
              ))}
              {!log && liveLines.length === 0 && <div className="text-slate-600">No worker has run for this task yet. Use Run Now on the card to start one.</div>}
              <div ref={bottom} />
            </>
          )}
        </div>
      </aside>
    </div>
  );
}
