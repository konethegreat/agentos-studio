import { Activity, ArrowUpRight, Bot, CircleAlert, CircleCheck, CircleHelp, FileText, Globe, Loader2 } from "lucide-react";
import { useEffect, useId, useMemo, useRef, useState } from "react";
import { chatEvidence, type ChatItem, type ToolItem } from "../lib/chat";
import { ToolCard } from "./ToolCard";

type EvidenceTab = "activity" | "sources" | "agents";

export function ToolEvidenceDialog({ tool, onClose }: { tool: ToolItem; onClose: () => void }) {
  const dialog = useRef<HTMLDialogElement>(null);
  const titleId = useId();
  useEffect(() => {
    const element = dialog.current;
    const opener = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    element?.showModal();
    return () => {
      element?.close();
      if (opener?.isConnected) opener.focus({ preventScroll: true });
    };
  }, []);
  return <dialog ref={dialog} aria-labelledby={titleId} onCancel={onClose} onClick={(event) => { if (event.target === event.currentTarget) onClose(); }} className="m-auto max-h-[85vh] w-[calc(100%_-_2rem)] max-w-2xl overflow-y-auto rounded-xl border border-line bg-card p-5 text-slate-200 shadow-xl backdrop:bg-black/60">
    <div className="mb-3 flex items-center justify-between gap-3"><h2 id={titleId} className="text-base font-semibold">Recorded tool evidence</h2><button type="button" onClick={onClose} className="rounded-lg px-3 py-1.5 text-small ring-1 ring-line hover:bg-white/5">Close</button></div>
    <p className="mb-3 text-small text-slate-500">The input and result recorded for this call. Opening this record does not execute the tool again.</p>
    <ToolCard key={tool.id} tool={tool} initiallyOpen />
  </dialog>;
}

function ResultState({ tool }: { tool: ToolItem }) {
  if (tool.status === "running") return <span className="inline-flex items-center gap-1 text-info"><Loader2 className="size-3 motion-safe:animate-spin" /> Call active</span>;
  if (tool.status === "error") return <span className="inline-flex items-center gap-1 text-danger"><CircleAlert className="size-3" /> Reported error</span>;
  if (tool.status === "unknown") return <span className="inline-flex items-center gap-1 text-slate-500"><CircleHelp className="size-3" /> No result recorded</span>;
  return <span className="inline-flex items-center gap-1 text-ok"><CircleCheck className="size-3" /> Result recorded</span>;
}

export function ChatEvidence({ items, streaming, backend, onInspect }: {
  items: ChatItem[];
  streaming: boolean;
  backend: string;
  onInspect: (tool: ToolItem) => void;
}) {
  const [tab, setTab] = useState<EvidenceTab>("activity");
  const evidence = useMemo(() => chatEvidence(items), [items]);
  const count = tab === "activity" ? evidence.tools.length : tab === "sources" ? evidence.sources.length : evidence.agents.length;
  const tabs = [
    { id: "activity" as const, label: "Activity", icon: Activity, count: evidence.tools.length },
    { id: "sources" as const, label: "Sources", icon: Globe, count: evidence.sources.length },
    { id: "agents" as const, label: "Agents", icon: Bot, count: evidence.agents.length },
  ];
  return (
    <section aria-label="Chat evidence" className="flex h-full min-h-0 flex-col bg-panel">
      <div className="border-b border-line px-4 py-4">
        <div className="flex items-center justify-between gap-2">
          <h2 className="text-sm font-semibold text-slate-100">Execution record</h2>
          <span className={`inline-flex items-center gap-1.5 text-label ${streaming ? "text-info" : "text-slate-500"}`}>
            {streaming && <span className="size-1.5 rounded-full bg-info motion-safe:animate-pulse" />}
            {streaming ? "Receiving events" : "Recorded events"}
          </span>
        </div>
        <p className="mt-1 text-small text-slate-500">{backend} · {evidence.running} active tool call{evidence.running === 1 ? "" : "s"}</p>
        <div aria-label="Evidence views" className="mt-4 flex gap-1 rounded-lg bg-inset p-1">
          {tabs.map(({ id, label, icon: Icon, count: total }) => (
            <button key={id} type="button" aria-pressed={tab === id} onClick={() => setTab(id)} className={`flex min-w-0 flex-1 items-center justify-center gap-1 rounded-md px-1 py-2 text-label transition-colors ${tab === id ? "bg-card font-semibold text-slate-100 shadow-sm ring-1 ring-line" : "text-slate-500 hover:text-slate-200"}`}>
              <Icon className="size-3 shrink-0" /> {label}<span className="tabular-nums text-slate-500">{total}</span>
            </button>
          ))}
        </div>
      </div>
      <div className="min-h-0 flex-1 overflow-y-auto px-4 py-3">
        {tab === "activity" && (
          <>
            <p className="mb-3 text-small leading-relaxed text-slate-500">Tool calls in this conversation, newest first. Open a call to inspect its input and result.</p>
            {evidence.tools.slice(-100).reverse().map((tool) => (
              <button key={tool.id} type="button" onClick={() => onInspect(tool)} className="group mb-2 block w-full rounded-lg border border-line bg-card px-3 py-3 text-left hover:border-slate-500">
                <span className="mb-1.5 flex items-start gap-2"><span className="min-w-0 flex-1 break-words font-mono text-xs text-slate-200">{tool.name}</span><ArrowUpRight className="size-3.5 shrink-0 text-slate-500 group-hover:text-info" /></span>
                <span className="mb-2 block truncate text-label text-slate-500">{tool.server?.replace(/_/g, "-") || "Built-in tool"}</span>
                <span className="flex flex-wrap justify-between gap-2 text-label"><ResultState tool={tool} />{tool.durationMs !== null && <span className="tabular-nums text-slate-500">{tool.durationMs.toLocaleString()} ms</span>}</span>
              </button>
            ))}
          </>
        )}
        {tab === "sources" && (
          <>
            <p className="mb-3 text-small leading-relaxed text-slate-500">File and web references from structured read requests. A reference is not proof that its contents were used. Open it to check the recorded result.</p>
            {evidence.sources.slice(-100).reverse().map((source) => {
              const Icon = source.kind === "web" ? Globe : FileText;
              return (
                <button key={`${source.tool.id}:${source.kind}:${source.value}`} type="button" onClick={() => onInspect(source.tool)} className="mb-2 block w-full rounded-lg border border-line bg-card px-3 py-3 text-left hover:border-slate-500">
                  <span className="mb-2 flex items-start gap-2"><Icon className="mt-0.5 size-3.5 shrink-0 text-info" /><span className="min-w-0 break-all font-mono text-small text-slate-200">{source.value}</span></span>
                  <span className="block text-label"><ResultState tool={source.tool} /></span>
                </button>
              );
            })}
          </>
        )}
        {tab === "agents" && (
          <>
            <p className="mb-3 text-small leading-relaxed text-slate-500">Recorded delegation calls. A returned call does not prove that a background agent has finished; inspect its result for reported progress.</p>
            {evidence.agents.slice(-100).reverse().map((agent) => (
              <button key={agent.tool.id} type="button" onClick={() => onInspect(agent.tool)} className="mb-2 block w-full rounded-lg border border-line bg-card px-3 py-3 text-left hover:border-slate-500">
                <span className="flex items-center gap-2 text-xs font-semibold text-slate-200"><Bot className="size-4 text-info" />{agent.label}</span>
                <span className="my-2 block line-clamp-3 break-words text-small leading-relaxed text-slate-400">{agent.task}</span>
                {agent.background && <span className="mb-2 block text-label text-slate-500">Background requested</span>}
                <span className="block text-label"><ResultState tool={agent.tool} /></span>
              </button>
            ))}
          </>
        )}
        {count === 0 && <div className="rounded-lg border border-dashed border-line px-4 py-6 text-center text-small leading-relaxed text-slate-500">{tab === "activity" ? "No tool calls recorded in this conversation." : tab === "sources" ? "No structured file or web reads recorded. Attached files are not counted as reads." : "No delegation calls recorded in this conversation."}</div>}
        {count > 100 && <p className="mt-3 text-label text-slate-500">Showing the latest 100 of {count}. The conversation retains earlier records.</p>}
      </div>
    </section>
  );
}
