import { invoke } from "@tauri-apps/api/core";
import { useEffect, useMemo, useState } from "react";
import { AlertTriangle, Brain, FileText, RefreshCw, Search, Sparkles, Upload } from "lucide-react";
import { api, errorText, type MemoryEntry, type MemoryNote } from "../lib/api";
import { useToast } from "../components/Toasts";
import { Guide, useReportGuides } from "../components/Guides";
import { HermesMemoryPanel } from "../components/HermesMemory";
import { LearningPanel } from "../components/Learning";
import { MemoryQualityPanel } from "../components/MemoryQuality";
import { SkillProposalsPanel } from "../components/SkillProposals";
import { WhyPanel } from "../components/Provenance";
import { EvaluationsPanel } from "../components/Evaluations";

const KIND_STYLE: Record<string, string> = {
  decision: "bg-emerald-500/10 text-emerald-300 ring-emerald-500/25",
  convention: "bg-sky-500/10 text-sky-300 ring-sky-500/25",
  gotcha: "bg-amber-500/10 text-amber-300 ring-amber-500/25",
  episode: "bg-violet-500/10 text-violet-300 ring-violet-500/25",
  fact: "bg-slate-500/10 text-slate-300 ring-slate-500/25",
};

function ago(secs: number): string {
  const d = Math.max(0, Math.floor(Date.now() / 1000) - secs);
  if (d < 60) return `${d}s ago`;
  if (d < 3600) return `${Math.floor(d / 60)}m ago`;
  if (d < 86_400) return `${Math.floor(d / 3600)}h ago`;
  return `${Math.floor(d / 86_400)}d ago`;
}

function stamp(secs: number): string {
  if (!secs) return "unknown time";
  return new Date(secs * 1000).toLocaleString();
}

function PaneError({ title, detail }: { title: string; detail: string }) {
  return (
    <div className="flex items-start gap-2 rounded-lg bg-amber-500/10 p-3 ring-1 ring-amber-500/25">
      <AlertTriangle className="mt-0.5 size-3.5 shrink-0 text-amber-300" />
      <div className="min-w-0 text-small">
        <div className="font-medium text-amber-200">{title}</div>
        <div className="mt-0.5 break-words text-amber-200/80">{detail}</div>
      </div>
    </div>
  );
}

/** One brain, two stores: the vault on the left and Hermes' own memory on the right.
 *  Wide windows show both columns; narrow windows switch between them with tabs. */
export function Memory() {
  const [notes, setNotes] = useState<MemoryNote[] | null>(null);
  const [recent, setRecent] = useState<MemoryEntry[]>([]);
  const [query, setQuery] = useState("");
  const [tab, setTab] = useState<"vault" | "hermes">("vault");
  const toast = useToast();
  const [busy, setBusy] = useState(false);
  const [vaultError, setVaultError] = useState<string | null>(null);
  const [notion, setNotion] = useState<{ connected: boolean; has_parent: boolean } | null>(null);

  async function load() {
    setBusy(true);
    setVaultError(null);
    try {
      const [n, r] = await Promise.all([api.memoryIndexNotes(), api.memoryRecent(60)]);
      setNotes(n);
      setRecent(r);
    } catch (e) {
      setVaultError(errorText(e));
    } finally {
      setBusy(false);
    }
  }

  useEffect(() => {
    void load();
    void api.notionStatus().then(setNotion).catch(() => undefined);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  async function publish() {
    setBusy(true);
    try {
      const r = await api.notionPublish(!r0connected());
      const changed = r.pages.filter((p) => !p.action.startsWith("unchanged"));
      toast(
        r.dry_run ? "info" : r.error ? "error" : "ok",
        r.dry_run
          ? `Dry run: ${changed.length} page(s) would change. Set NOTION_API_KEY to publish.`
          : r.error ?? `Published ${changed.length} page(s) to Notion`,
      );
    } catch (e) {
      toast("error", errorText(e));
    } finally {
      setBusy(false);
    }
  }

  function r0connected() {
    return notion?.connected ?? false;
  }

  async function syncBoard() {
    setBusy(true);
    try {
      const r = await invoke<{ dry_run: boolean; error: string | null; created_rows: number; updated_rows: number; unchanged_rows: number; proposed: string[]; awaiting_approval: string[]; cancelled: string[]; declined: string[] }>("notion_sync_missions", { dryRun: !r0connected() });
      if (r.error) toast("error", r.error);
      else
        toast(
          r.dry_run ? "info" : "ok",
          `${r.dry_run ? "Dry run: " : ""}${r.created_rows} row(s) new, ${r.updated_rows} updated, ${r.unchanged_rows} unchanged` +
            (r.proposed.length ? `; from Notion: ${r.proposed.join(", ")}` : "") +
            (r.awaiting_approval.length ? ` (${r.awaiting_approval.length} waiting for approval in Missions)` : "") +
            (r.cancelled.length ? `; cancelled: ${r.cancelled.join(", ")}` : "") +
            (r.declined.length ? `; declined: ${r.declined.length} (reason written to the row)` : ""),
        );
    } catch (e) {
      toast("error", errorText(e));
    } finally {
      setBusy(false);
    }
  }

  async function syncGoals() {
    setBusy(true);
    try {
      const r = await invoke<{ dry_run: boolean; error: string | null; created_rows: number; updated_rows: number; unchanged_rows: number; archived_rows: number; proposed: string[]; held_at_watch: string[]; decided: string[]; declined: string[] }>("notion_sync_goals", { dryRun: !r0connected() });
      if (r.error) toast("error", r.error);
      else
        toast(
          r.dry_run ? "info" : "ok",
          `${r.dry_run ? "Dry run: " : ""}${r.created_rows} goal row(s) new, ${r.updated_rows} updated, ${r.unchanged_rows} unchanged` +
            (r.archived_rows ? `, ${r.archived_rows} archived` : "") +
            (r.proposed.length ? `; from Notion: ${r.proposed.join(", ")}` : "") +
            (r.held_at_watch.length ? ` (${r.held_at_watch.length} held at Watch until approved in Goals)` : "") +
            (r.decided.length ? `; answered: ${r.decided.join(", ")}` : "") +
            (r.declined.length ? `; declined: ${r.declined.length} (reason written to the row)` : ""),
        );
    } catch (e) {
      toast("error", errorText(e));
    } finally {
      setBusy(false);
    }
  }

  async function pullInbox() {
    setBusy(true);
    try {
      const n = await api.notionPullInbox();
      toast("ok", `${n} note(s) pulled into 00_Inbox`);
      await load();
    } catch (e) {
      toast("error", errorText(e));
    } finally {
      setBusy(false);
    }
  }

  const filtered = useMemo(() => {
    const list = notes ?? [];
    const q = query.trim().toLowerCase();
    if (!q) return list;
    return list.filter((n) => n.title.toLowerCase().includes(q) || n.path.toLowerCase().includes(q));
  }, [notes, query]);

  const counts = useMemo(() => {
    const c: Record<string, number> = {};
    for (const n of notes ?? []) c[n.kind] = (c[n.kind] ?? 0) + 1;
    return c;
  }, [notes]);

  useReportGuides("memory", notes !== null && notes.length === 0 ? ["memory.empty"] : []);

  return (
    <div className="flex h-full flex-col">
      <Guide id="memory.empty" className="m-3 mb-0" action={{ label: "Index again", run: () => void load() }} />
      <div className="flex flex-wrap items-center gap-3 border-b border-line px-4 py-3">
        <Brain className="size-4 text-emerald-300" />
        <div className="text-sm font-semibold text-slate-100">Brain</div>
        <div className="flex rounded-lg bg-white/[0.04] p-0.5 ring-1 ring-line xl:hidden">
          {(
            [
              ["vault", "Vault"],
              ["hermes", "Hermes"],
            ] as const
          ).map(([id, label]) => (
            <button
              key={id}
              onClick={() => setTab(id)}
              className={`h-7 rounded-md px-2.5 text-xs font-medium ${tab === id ? "bg-emerald-500/15 text-emerald-300" : "text-slate-400 hover:text-slate-200"}`}
            >
              {label}
            </button>
          ))}
        </div>
        <span className="text-label text-slate-500">
          {vaultError ? "vault unavailable" : notes === null ? "indexing" : `${notes.length} notes in the index`}
        </span>
        <div className={`relative ml-auto ${tab === "hermes" ? "hidden xl:block" : ""}`}>
          <Search className="pointer-events-none absolute left-2 top-1/2 size-3.5 -translate-y-1/2 text-slate-500" />
          <input
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder="Filter notes"
            className="h-8 w-56 rounded-lg bg-white/[0.03] pl-7 pr-2 text-xs text-slate-200 outline-none ring-1 ring-line focus:ring-emerald-500/40"
          />
        </div>
        <div className="flex items-center gap-1.5">
          <button
            onClick={() => void publish()}
            disabled={busy}
            title={notion?.connected ? "Publish the digest, tasks, missions and fleet" : "No NOTION_API_KEY set: this runs a dry run"}
            className="inline-flex h-8 items-center gap-1.5 rounded-lg px-2.5 text-xs text-slate-300 ring-1 ring-line hover:bg-white/[0.04] disabled:opacity-50"
          >
            <Upload className="size-3.5" /> {notion?.connected ? "Publish" : "Dry run"}
          </button>
          <button
            onClick={() => void syncBoard()}
            disabled={busy}
            title={notion?.connected ? "Two-way missions board in Notion: publishes every mission as a row; a new row with Goal, Repo and Acceptance becomes a mission (queued if its author is trusted, otherwise waiting for approval in Missions), and Request = Cancel cancels a queued or blocked one. Status, verdict and merges stay in Studio. Runs every 5 minutes while Automations are on." : "No NOTION_API_KEY set: this runs a dry run"}
            className="inline-flex h-8 items-center rounded-lg px-2.5 text-xs text-slate-300 ring-1 ring-line hover:bg-white/[0.04] disabled:opacity-50"
          >
            Missions board
          </button>
          <button
            onClick={() => void syncGoals()}
            disabled={busy}
            title={notion?.connected ? "Two-way goals board in Notion: publishes every goal with its progress, spend and open question; a new row with Goal, Repos and Done checks becomes a goal (planned at once if its author is trusted, otherwise held at Watch), and a Decision from a trusted author answers the open escalation. Status, checks and merges stay in Studio. Runs every 5 minutes once the board exists." : "No NOTION_API_KEY set: this runs a dry run"}
            className="inline-flex h-8 items-center rounded-lg px-2.5 text-xs text-slate-300 ring-1 ring-line hover:bg-white/[0.04] disabled:opacity-50"
          >
            Goals board
          </button>
          {notion?.connected && (
            <button
              onClick={() => void pullInbox()}
              disabled={busy}
              title="Pull the Notion inbox database into 00_Inbox"
              className="inline-flex h-8 items-center rounded-lg px-2.5 text-xs text-slate-300 ring-1 ring-line hover:bg-white/[0.04]"
            >
              Inbox
            </button>
          )}
        </div>
        <button
          onClick={() => void load()}
          disabled={busy}
          className="inline-flex h-8 items-center gap-1.5 rounded-lg px-2.5 text-xs text-slate-300 ring-1 ring-line hover:bg-white/[0.04] disabled:opacity-50"
        >
          <RefreshCw className={`size-3.5 ${busy ? "animate-spin" : ""}`} /> Refresh
        </button>
      </div>

      <div className="grid min-h-0 flex-1 grid-cols-1 xl:grid-cols-2">
        <section
          className={`${tab === "hermes" ? "hidden xl:flex" : "flex"} min-h-0 flex-col xl:border-r xl:border-line`}
        >
          <div className="flex flex-wrap items-center gap-1.5 border-b border-line px-4 py-2">
            <span className="rounded-full bg-emerald-500/10 px-2 py-0.5 text-caption font-medium text-emerald-300 ring-1 ring-emerald-500/25">
              Vault
            </span>
            {["decision", "convention", "gotcha", "episode", "fact"].map((k) => (
              <span
                key={k}
                className={`rounded-full px-2 py-0.5 text-caption font-medium ring-1 ${KIND_STYLE[k]}`}
              >
                {k} {counts[k] ?? 0}
              </span>
            ))}
          </div>
          <div className="min-h-0 flex-1 overflow-y-auto">
            {vaultError ? (
              <div className="space-y-3 p-4">
                <PaneError title="The vault index is unavailable" detail={vaultError} />
                <button
                  onClick={() => void load()}
                  className="inline-flex h-8 items-center gap-1.5 rounded-lg px-2.5 text-xs text-slate-300 ring-1 ring-line hover:bg-white/[0.04]"
                >
                  <RefreshCw className="size-3.5" /> Try again
                </button>
              </div>
            ) : notes === null ? (
              <div className="p-6 text-sm text-slate-500">Reading the vault...</div>
            ) : filtered.length === 0 ? (
              <div className="p-6 text-sm text-slate-500">
                No notes match. Memories written by an agent through `remember` appear here.
              </div>
            ) : (
              <ul className="divide-y divide-line/60">
                {filtered.map((n) => (
                  <li key={n.path} className="px-4 py-2.5 hover:bg-white/[0.02]">
                    <div className="flex items-center gap-2">
                      <FileText className="size-3.5 shrink-0 text-slate-500" />
                      <span className="truncate text-body-sm font-medium text-slate-200">{n.title || n.path}</span>
                      <span className="shrink-0 rounded-full bg-emerald-500/10 px-1.5 py-0.5 text-2xs font-medium text-emerald-300 ring-1 ring-emerald-500/25">
                        Vault
                      </span>
                      <span className={`shrink-0 rounded-full px-1.5 py-0.5 text-2xs font-medium ring-1 ${KIND_STYLE[n.kind] ?? KIND_STYLE.fact}`}>
                        {n.kind}
                      </span>
                      {n.status === "superseded" && (
                        <span className="shrink-0 rounded-full bg-slate-600/20 px-1.5 py-0.5 text-2xs text-slate-400 ring-1 ring-slate-500/30">
                          superseded
                        </span>
                      )}
                      <span className="ml-auto shrink-0 text-caption text-slate-500" title="Notes linking here">
                        {n.in_degree} link{n.in_degree === 1 ? "" : "s"}
                      </span>
                    </div>
                    <div className="mt-0.5 flex items-center gap-2 pl-5">
                      <span className="truncate font-mono text-caption text-slate-500">{n.path}</span>
                      <span className="ml-auto shrink-0 text-caption text-slate-500" title={stamp(n.mtime)}>
                        changed {n.mtime ? ago(n.mtime) : "unknown"}
                      </span>
                    </div>
                  </li>
                ))}
              </ul>
            )}
          </div>
          <MemoryQualityPanel onChanged={() => void load()} />
          <LearningPanel onWrote={() => void load()} />
          <SkillProposalsPanel />
          <WhyPanel />
          <EvaluationsPanel />
          <div className="max-h-64 shrink-0 overflow-y-auto border-t border-line">
            <div className="flex items-center gap-2 border-b border-line px-4 py-2 text-small font-medium text-slate-400">
              <Sparkles className="size-3.5 text-violet-300" /> Recent memory activity
            </div>
            {recent.length === 0 ? (
              <div className="p-4 text-xs text-slate-500">
                No chat turn has used memory yet. Reads and writes are recorded from tool calls, so this fills as you work.
              </div>
            ) : (
              <ul className="divide-y divide-line/60">
                {recent.map((e, i) => (
                  <li key={`${e.session_id}-${e.turn}-${i}`} className="px-4 py-2 text-small">
                    <div className="flex items-center gap-2">
                      <span className={e.op === "write" ? "text-amber-300" : "text-sky-300"}>{e.op}</span>
                      <span className="text-slate-500">{e.target}</span>
                      <span className="truncate font-mono text-caption text-slate-300" title={e.path}>
                        {e.path || e.tool}
                      </span>
                      <span className="ml-auto shrink-0 text-2xs text-slate-500">{ago(e.at)}</span>
                    </div>
                    <div className="truncate text-2xs text-slate-600">
                      turn {e.turn} in {e.session_id}
                    </div>
                  </li>
                ))}
              </ul>
            )}
          </div>
        </section>

        <section
          className={`${tab === "vault" ? "hidden xl:flex" : "flex"} min-h-0 flex-col overflow-hidden`}
        >
          <HermesMemoryPanel />
        </section>
      </div>
    </div>
  );
}
