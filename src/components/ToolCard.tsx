import { ChevronRight, CircleAlert, CircleCheck, Loader2, Plug, Wrench } from "lucide-react";
import { useState } from "react";
import type { ToolItem } from "../lib/chat";

const SERVER_TONES: Record<string, string> = {
  agentos_hub: "bg-emerald-500/10 text-emerald-300 ring-emerald-500/25",
  "agentos-hub": "bg-emerald-500/10 text-emerald-300 ring-emerald-500/25",
  notion: "bg-slate-200/10 text-slate-200 ring-slate-300/25",
};

function clip(s: string, max: number) {
  return s.length > max ? `${s.slice(0, max)}\n... ${s.length - max} more characters` : s;
}

/** Collapsible card for one MCP or built-in tool call: which server, which tool, what went in, what came out. */
export function ToolCard({ tool }: { tool: ToolItem }) {
  const [open, setOpen] = useState(false);
  const tone = tool.server ? (SERVER_TONES[tool.server] ?? "bg-sky-500/10 text-sky-300 ring-sky-500/25") : "bg-white/5 text-slate-300 ring-white/10";
  const firstArg = (() => {
    try {
      const v = JSON.parse(tool.input || "{}") as Record<string, unknown>;
      const k = Object.keys(v)[0];
      return k ? `${k}: ${typeof v[k] === "string" ? v[k] : JSON.stringify(v[k])}` : "";
    } catch {
      return tool.input.split("\n")[0] ?? "";
    }
  })();
  return (
    <div className={`my-2 overflow-hidden rounded-xl border bg-card/60 ${tool.status === "error" ? "border-red-500/30" : "border-line"}`}>
      <button onClick={() => setOpen((o) => !o)} className="flex w-full items-center gap-2.5 px-3 py-2 text-left hover:bg-white/[0.02]">
        <ChevronRight className={`size-3.5 shrink-0 text-slate-500 transition-transform ${open ? "rotate-90" : ""}`} />
        {tool.status === "running" ? (
          <Loader2 className="size-3.5 shrink-0 animate-spin text-sky-300" />
        ) : tool.status === "error" ? (
          <CircleAlert className="size-3.5 shrink-0 text-red-400" />
        ) : (
          <CircleCheck className="size-3.5 shrink-0 text-emerald-400" />
        )}
        <span className={`inline-flex shrink-0 items-center gap-1 rounded-md px-1.5 py-0.5 text-[10.5px] font-medium ring-1 ring-inset ${tone}`}>
          {tool.server ? <Plug className="size-3" /> : <Wrench className="size-3" />}
          {tool.server ? tool.server.replace(/_/g, "-") : "built-in"}
        </span>
        <span className="shrink-0 font-mono text-xs text-slate-200">{tool.name}</span>
        <span className="min-w-0 flex-1 truncate font-mono text-[11px] text-slate-500">{firstArg}</span>
        {tool.durationMs !== null && <span className="shrink-0 text-[11px] tabular-nums text-slate-500">{tool.durationMs} ms</span>}
      </button>
      {open && (
        <div className="space-y-2 border-t border-line px-3 py-2.5">
          {tool.input && (
            <div>
              <div className="mb-1 text-[10.5px] font-semibold uppercase tracking-wide text-slate-500">Input</div>
              <pre className="max-h-60 overflow-auto rounded-lg bg-canvas p-2.5 font-mono text-[11.5px] text-slate-300">{clip(tool.input, 6000)}</pre>
            </div>
          )}
          <div>
            <div className="mb-1 text-[10.5px] font-semibold uppercase tracking-wide text-slate-500">{tool.status === "error" ? "Error" : "Result"}</div>
            <pre
              className={`max-h-72 overflow-auto whitespace-pre-wrap break-words rounded-lg bg-canvas p-2.5 font-mono text-[11.5px] ${tool.status === "error" ? "text-red-300" : "text-slate-300"}`}
            >
              {tool.output === null ? (tool.status === "running" ? "Running..." : "No output recorded") : clip(tool.output, 12000) || "(empty)"}
            </pre>
          </div>
        </div>
      )}
    </div>
  );
}
