import { CircleCheck, Download, Monitor, Moon, RefreshCw, SquareTerminal, Sun, TriangleAlert } from "lucide-react";
import { NEXT_PREF, setThemePref, themePref, type ThemePref } from "../lib/theme";
import type { ReactNode } from "react";
import type { HermesStatus, StackStatus } from "../lib/api";
import { useCli } from "./CliDrawer";
import { api } from "../lib/api";
import { useEffect, useState } from "react";

function Chip({ children, title, onClick, tone = "" }: { children: ReactNode; title?: string; onClick?: () => void; tone?: string }) {
  return (
    <button
      type="button"
      title={title}
      onClick={onClick}
      className={`inline-flex h-7 min-w-0 items-center gap-1.5 rounded-lg px-2.5 text-small ring-1 ring-line transition-colors hover:bg-white/[0.04] ${tone || "text-slate-300"}`}
    >
      {children}
    </button>
  );
}

/**
 * Status bar. Provider and model live in the chat header only, so this bar answers one question:
 * is the machine healthy. The health chip is the single way into Diagnostics.
 */
const THEME_LABEL: Record<ThemePref, string> = { system: "Theme: follow Windows", dark: "Theme: dark", light: "Theme: light" };

/** Cycles follow Windows, dark and light. The choice is applied at once and before the next start. */
function ThemeButton() {
  const [pref, setPref] = useState<ThemePref>(() => themePref());
  const Icon = pref === "system" ? Monitor : pref === "dark" ? Moon : Sun;
  const next = NEXT_PREF[pref];
  return (
    <button
      onClick={() => {
        setThemePref(next);
        setPref(next);
      }}
      title={`${THEME_LABEL[pref]}. Click for ${THEME_LABEL[next].replace("Theme: ", "")}.`}
      aria-label={THEME_LABEL[pref]}
      className="grid size-8 place-items-center rounded-lg text-slate-400 ring-1 ring-line hover:bg-white/[0.04] hover:text-slate-200"
    >
      <Icon className="size-3.5" />
    </button>
  );
}

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
  const [available, setAvailable] = useState<string | null>(null);
  const [installing, setInstalling] = useState(false);
  const [offline, setOffline] = useState<{ online: boolean; local_models: string[]; note: string } | null>(null);

  useEffect(() => {
    void api.studioUpdateCheck().then(setAvailable).catch(() => undefined);
    void api.offlineStatus().then(setOffline).catch(() => undefined);
  }, []);

  async function install() {
    setInstalling(true);
    try {
      await api.studioUpdateInstall();
      setAvailable(null);
    } catch {
      setInstalling(false);
    }
  }
  const hermesOk = Boolean(hermes?.installed && hermes.config_ok);
  const systemsOk = Boolean(stack && stack.connected === stack.total);
  const healthy = hermesOk && systemsOk;
  const missing = stack ? Math.max(0, stack.total - stack.connected) : 0;
  const needs = healthy ? 0 : missing + (hermesOk ? 0 : 1);

  return (
    <div className="flex h-12 shrink-0 items-center gap-2 border-b border-line bg-inset px-4">
      <Chip
        title={hermes?.hermes_path ?? "Hermes not found"}
        onClick={onOpenDiagnostics}
        tone={hermesOk ? "text-slate-200" : "text-amber-300"}
      >
        <span className={`size-2 shrink-0 rounded-full ${hermesOk ? "bg-emerald-400 shadow-[0_0_8px_rgba(52,211,153,0.8)]" : "bg-amber-400"}`} />
        Hermes {hermes ? (hermes.installed ? (hermes.version ?? "installed") : "not installed") : "connecting"}
      </Chip>
      {hermes?.claude_version && (
        <span className="hidden text-label text-slate-500 xl:inline" title={hermes.claude_path ?? ""}>
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
      {offline && !offline.online && (
        <span
          title={offline.note}
          className="inline-flex h-7 items-center gap-1.5 rounded-lg bg-amber-500/10 px-2.5 text-label text-amber-300 ring-1 ring-amber-500/25"
        >
          Offline{offline.local_models.length > 0 ? ` · ${offline.local_models.length} local` : ""}
        </span>
      )}
      {available && (
        <button
          onClick={() => void install()}
          disabled={installing}
          title="Signed update: the signature is checked before anything installs"
          className="inline-flex h-8 items-center gap-1.5 rounded-lg bg-sky-500/15 px-3 text-xs font-semibold text-sky-200 ring-1 ring-sky-500/30 hover:bg-sky-500/20 disabled:opacity-50"
        >
          <Download className="size-3.5" />
          {installing ? "Installing" : `Update to ${available}`}
        </button>
      )}
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
      <ThemeButton />
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
