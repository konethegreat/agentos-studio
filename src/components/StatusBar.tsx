import { CircleCheck, RefreshCw, SquareTerminal, TriangleAlert } from "lucide-react";
import type { ReactNode } from "react";
import type { HermesStatus, StackStatus } from "../lib/api";
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

/**
 * Status bar. Provider and model live in the chat header only, so this bar answers one question:
 * is the machine healthy. The health chip is the single way into Diagnostics.
 */
export function StatusBar({
  hermes,
  stack,
  onOpenDiagnostics,
  onRefresh,
}: {
  hermes: HermesStatus | null;
  stack: StackStatus | null;
  onOpenDiagnostics: () => void;
  onRefresh: () => void;
}) {
  const cli = useCli();
  const hermesOk = Boolean(hermes?.installed && hermes.config_ok);
  const systemsOk = Boolean(stack && stack.connected === stack.total);
  const healthy = hermesOk && systemsOk;
  const missing = stack ? Math.max(0, stack.total - stack.connected) : 0;
  const needs = healthy ? 0 : missing + (hermesOk ? 0 : 1);

  return (
    <div className="flex h-12 shrink-0 items-center gap-2 border-b border-line bg-[#0a0e15] px-4">
      <Chip
        title={hermes?.hermes_path ?? "Hermes not found"}
        onClick={onOpenDiagnostics}
        tone={hermesOk ? "text-slate-200" : "text-amber-300"}
      >
        <span className={`size-2 shrink-0 rounded-full ${hermesOk ? "bg-emerald-400 shadow-[0_0_8px_rgba(52,211,153,0.8)]" : "bg-amber-400"}`} />
        Hermes {hermes ? (hermes.installed ? (hermes.version ?? "installed") : "not installed") : "connecting"}
      </Chip>
      {hermes?.claude_version && (
        <span className="hidden text-[11px] text-slate-500 xl:inline" title={hermes.claude_path ?? ""}>
          Claude Code {hermes.claude_version}
        </span>
      )}
      <button
        onClick={onRefresh}
        title="Refresh status"
        aria-label="Refresh status"
        className="rounded-md p-1.5 text-slate-500 hover:bg-white/5 hover:text-slate-200"
      >
        <RefreshCw className="size-3.5" />
      </button>
      <div className="flex-1" />
      <button
        onClick={onOpenDiagnostics}
        title={healthy ? "Every check passed. Open Diagnostics for the report." : `${needs} check${needs === 1 ? "" : "s"} need attention. Open Diagnostics.`}
        className={`inline-flex h-8 items-center gap-2 rounded-lg px-3 text-xs font-semibold ring-1 transition-colors ${
          healthy
            ? "bg-emerald-500/10 text-emerald-300 ring-emerald-500/30 hover:bg-emerald-500/15"
            : "bg-amber-500/10 text-amber-300 ring-amber-500/30 hover:bg-amber-500/15"
        }`}
      >
        {healthy ? <CircleCheck className="size-4" /> : <TriangleAlert className="size-4" />}
        {healthy ? "Healthy" : stack ? `${needs} need attention` : "Checking"}
      </button>
      <button
        onClick={() => cli.setOpen(!cli.open)}
        title="Terminal (Ctrl+`)"
        className={`inline-flex h-8 items-center gap-1.5 rounded-lg px-2.5 text-xs ring-1 transition-colors ${cli.open ? "bg-emerald-500/10 text-emerald-300 ring-emerald-500/30" : "text-slate-300 ring-line hover:bg-white/[0.04]"}`}
      >
        <SquareTerminal className="size-3.5" /> Terminal
      </button>
    </div>
  );
}
