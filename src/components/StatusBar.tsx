import { Bot, Cpu, Plug, RefreshCw, SquareTerminal } from "lucide-react";
import type { ReactNode } from "react";
import type { HermesStatus, StackStatus } from "../lib/api";
import { HealthPill } from "./CommandBar";
import { useCli } from "./CliDrawer";

function Chip({ children, title, onClick, tone = "" }: { children: ReactNode; title?: string; onClick?: () => void; tone?: string }) {
  return (
    <button
      type="button"
      title={title}
      onClick={onClick}
      className={`inline-flex h-7 min-w-0 items-center gap-1.5 rounded-lg px-2.5 text-[11.5px] ring-1 ring-line transition-colors hover:bg-white/[0.04] ${tone || "text-slate-300"}`}
    >
      {children}
    </button>
  );
}

/** Always-visible heartbeat: Hermes version, active provider and model, MCP servers, system health. */
export function StatusBar({
  hermes,
  stack,
  onOpenSetup,
  onOpenControl,
  onRefresh,
}: {
  hermes: HermesStatus | null;
  stack: StackStatus | null;
  onOpenSetup: () => void;
  onOpenControl: (tab: "providers" | "mcp") => void;
  onRefresh: () => void;
}) {
  const cli = useCli();
  const alive = Boolean(hermes?.installed && hermes.config_ok);
  return (
    <div className="flex h-12 shrink-0 items-center gap-2 border-b border-line bg-[#0a0e15] px-4">
      <Chip title={hermes?.hermes_path ?? "Hermes not found"} onClick={onOpenSetup} tone={alive ? "text-slate-200" : "text-amber-300"}>
        <span className={`size-2 shrink-0 rounded-full ${alive ? "bg-emerald-400 shadow-[0_0_8px_rgba(52,211,153,0.8)]" : "bg-amber-400"}`} />
        Hermes {hermes ? (hermes.installed ? (hermes.version ?? "installed") : "not installed") : "connecting"}
      </Chip>
      <Chip title="Switch provider" onClick={() => onOpenControl("providers")}>
        <Bot className="size-3.5 shrink-0 text-emerald-300" />
        <span className="truncate">{hermes?.provider_label || "No provider"}</span>
      </Chip>
      <Chip title="Active model" onClick={() => onOpenControl("providers")}>
        <Cpu className="size-3.5 shrink-0 text-sky-300" />
        <span className="max-w-56 truncate font-mono text-[11px]">{hermes?.model || "no model"}</span>
      </Chip>
      <Chip title="MCP servers enabled in Hermes" onClick={() => onOpenControl("mcp")}>
        <Plug className="size-3.5 shrink-0 text-violet-300" />
        {hermes ? `${hermes.mcp_enabled}/${hermes.mcp_total} MCP` : "MCP"}
      </Chip>
      {hermes?.claude_version && (
        <span className="hidden text-[11px] text-slate-500 xl:inline" title={hermes.claude_path ?? ""}>
          Claude Code {hermes.claude_version}
        </span>
      )}
      <button onClick={onRefresh} title="Refresh status" aria-label="Refresh status" className="rounded-md p-1.5 text-slate-500 hover:bg-white/5 hover:text-slate-200">
        <RefreshCw className="size-3.5" />
      </button>
      <div className="flex-1" />
      <button
        onClick={() => cli.setOpen(!cli.open)}
        title="Terminal (Ctrl+`)"
        className={`inline-flex h-8 items-center gap-1.5 rounded-lg px-2.5 text-xs ring-1 transition-colors ${cli.open ? "bg-emerald-500/10 text-emerald-300 ring-emerald-500/30" : "text-slate-300 ring-line hover:bg-white/[0.04]"}`}
      >
        <SquareTerminal className="size-3.5" /> Terminal
      </button>
      <div className="[&>*]:h-8 [&>*]:rounded-lg">
        <HealthPill status={stack} onOpenSetup={onOpenSetup} />
      </div>
    </div>
  );
}
