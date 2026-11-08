import { Check, FileText, Loader2, RefreshCw, Sparkles, X } from "lucide-react";
import { useCallback, useEffect, useState } from "react";
import { api, errorText, type SkillProposal } from "../lib/api";
import { useToast } from "./Toasts";
import { ApprovalDeclined, useGuarded } from "./ApprovalProvider";

/** Approved missions that share a role and a task class become a skill draft. Nothing is
 *  installed until the owner approves, and approval runs through the permission check. */
export function SkillProposalsPanel() {
  const toast = useToast();
  const guarded = useGuarded();
  const [proposals, setProposals] = useState<SkillProposal[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [open, setOpen] = useState<string | null>(null);

  const load = useCallback(() => {
    setError(null);
    void api
      .skillProposals()
      .then(setProposals)
      .catch((e) => setError(errorText(e)));
  }, []);

  useEffect(() => {
    load();
  }, [load]);

  async function approve(p: SkillProposal) {
    setBusy(p.hash);
    try {
      const path = await guarded((a) => api.skillProposalApprove(p.hash, a.approved));
      toast("ok", `Skill ${p.name} created at ${path}`);
      load();
    } catch (e) {
      if (!(e instanceof ApprovalDeclined)) toast("error", errorText(e));
    } finally {
      setBusy(null);
    }
  }

  async function dismiss(p: SkillProposal) {
    setBusy(p.hash);
    try {
      await api.skillProposalDismiss(p.hash);
      toast("info", `${p.name} dismissed; it will not be proposed again.`);
      load();
    } catch (e) {
      toast("error", errorText(e));
    } finally {
      setBusy(null);
    }
  }

  return (
    <div className="max-h-72 shrink-0 overflow-y-auto border-t border-line">
      <div className="flex items-center gap-2 border-b border-line px-4 py-2 text-small font-medium text-slate-400">
        <Sparkles className="size-3.5 text-violet-300" />
        Skill proposals
        <span className="text-caption font-normal text-slate-500">
          three approved missions on one procedure
        </span>
        <button
          onClick={load}
          disabled={busy !== null}
          aria-label="Reload skill proposals"
          title="Reload skill proposals"
          className="ml-auto inline-flex size-7 items-center justify-center rounded-lg text-slate-400 ring-1 ring-line hover:bg-white/[0.04] disabled:opacity-50"
        >
          <RefreshCw className="size-3.5" />
        </button>
      </div>
      {error ? (
        <div className="px-4 py-3 text-xs text-amber-200">{error}</div>
      ) : proposals === null ? (
        <div className="px-4 py-3 text-xs text-slate-500">Reading approved missions...</div>
      ) : proposals.length === 0 ? (
        <div className="px-4 py-3 text-xs text-slate-500">
          No proposal yet. A proposal appears when three approved missions share a role and the same
          task class.
        </div>
      ) : (
        <ul className="divide-y divide-line/60">
          {proposals.map((p) => (
            <li key={p.hash} className="px-4 py-2">
              <div className="flex items-center gap-2">
                <FileText className="size-3.5 shrink-0 text-violet-300" />
                <span className="truncate font-mono text-small text-slate-200" title={p.name}>
                  {p.name}
                </span>
                <span className="shrink-0 rounded-full bg-white/[0.04] px-1.5 py-0.5 text-2xs text-slate-400 ring-1 ring-line">
                  {p.role}
                </span>
                <span className="shrink-0 text-2xs text-slate-500">
                  {p.evidence.length} mission{p.evidence.length === 1 ? "" : "s"}
                </span>
                <button
                  onClick={() => setOpen(open === p.hash ? null : p.hash)}
                  className="ml-auto inline-flex h-7 shrink-0 items-center rounded-lg px-2 text-xs text-slate-300 ring-1 ring-line hover:bg-white/[0.04]"
                >
                  {open === p.hash ? "Hide draft" : "Draft"}
                </button>
                <button
                  onClick={() => void approve(p)}
                  disabled={busy !== null}
                  title="Create this skill in Hermes through one turn; asks for approval first"
                  className="inline-flex h-7 shrink-0 items-center gap-1 rounded-lg bg-emerald-500/10 px-2 text-xs text-emerald-300 ring-1 ring-emerald-500/25 hover:bg-emerald-500/15 disabled:opacity-50"
                >
                  {busy === p.hash ? <Loader2 className="size-3.5 animate-spin" /> : <Check className="size-3.5" />}
                  Approve
                </button>
                <button
                  onClick={() => void dismiss(p)}
                  disabled={busy !== null}
                  title="Never propose this group again"
                  className="inline-flex h-7 shrink-0 items-center gap-1 rounded-lg px-2 text-xs text-slate-400 ring-1 ring-line hover:bg-white/[0.04] disabled:opacity-50"
                >
                  <X className="size-3.5" />
                  Dismiss
                </button>
              </div>
              <div className="mt-0.5 truncate pl-5 text-caption text-slate-500" title={p.description}>
                {p.description}
              </div>
              {p.evidence.length > 0 && (
                <div className="mt-0.5 truncate pl-5 text-2xs text-slate-600">
                  {p.evidence.map((e) => e.mission).join(", ")}
                </div>
              )}
              {open === p.hash && (
                <pre className="mt-2 max-h-60 overflow-auto whitespace-pre-wrap rounded-lg bg-black/30 p-2 font-mono text-2xs text-slate-300 ring-1 ring-line">
                  {p.draft}
                </pre>
              )}
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
