export function relativeTime(iso: string | null | undefined): string {
  if (!iso) return "";
  const t = Date.parse(iso);
  if (Number.isNaN(t)) return iso;
  const s = Math.round((Date.now() - t) / 1000);
  if (s < 45) return "just now";
  const m = Math.round(s / 60);
  if (m < 60) return `${m}m ago`;
  const h = Math.round(m / 60);
  if (h < 36) return `${h}h ago`;
  const d = Math.round(h / 24);
  if (d < 60) return `${d}d ago`;
  return new Date(t).toLocaleDateString();
}

export const AGENT_LABEL: Record<string, string> = {
  hermes: "Hermes",
  "claude-code": "Claude Code",
  qoder: "Qoder",
  antigravity: "Antigravity",
  human: "Human",
};

export const AGENT_COLOR: Record<string, string> = {
  hermes: "bg-violet-500/15 text-violet-300 ring-violet-500/30",
  "claude-code": "bg-orange-500/15 text-orange-300 ring-orange-500/30",
  qoder: "bg-sky-500/15 text-sky-300 ring-sky-500/30",
  antigravity: "bg-pink-500/15 text-pink-300 ring-pink-500/30",
  human: "bg-slate-500/15 text-slate-300 ring-slate-500/30",
};

export const PRIORITY_COLOR: Record<string, string> = {
  critical: "bg-red-500/15 text-red-300 ring-red-500/30",
  high: "bg-amber-500/15 text-amber-300 ring-amber-500/30",
  medium: "bg-sky-500/10 text-sky-300 ring-sky-500/25",
  low: "bg-slate-500/10 text-slate-400 ring-slate-500/25",
};

export function basename(p: string): string {
  const parts = p.split(/[\\/]/).filter(Boolean);
  return parts[parts.length - 1] ?? p;
}
