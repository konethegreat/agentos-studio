import { useCallback, useEffect, useState } from "react";
import {
  Archive,
  CheckCircle2,
  FileText,
  FlaskConical,
  RefreshCw,
  Search,
  Sparkles,
} from "lucide-react";
import {
  api,
  errorText,
  type ContextEvalReport,
  type ContextEvent,
  type ContextHit,
  type ContextProbeScore,
} from "../lib/api";
import { useToast } from "../components/Toasts";

function fmtScore(n: number): string {
  return `${Math.round(n * 100)}%`;
}

function fmtTokens(n: number): string {
  if (n >= 1000) return `${(n / 1000).toFixed(1)}k`;
  return `${n}`;
}

function ScorePills({ s, bar = 0.8 }: { s: ContextProbeScore; bar?: number }) {
  const items: [string, number][] = [
    ["facts", s.facts],
    ["artifacts", s.artifacts],
    ["cont", s.continuation],
    ["decisions", s.decisions],
  ];
  return (
    <div className="flex flex-wrap gap-1.5 text-2xs">
      {items.map(([k, v]) => (
        <span
          key={k}
          className={`rounded px-1.5 py-0.5 font-mono ${
            v >= bar
              ? "bg-emerald-500/15 text-emerald-300"
              : "bg-amber-500/15 text-amber-300"
          }`}
        >
          {k} {fmtScore(v)}
        </span>
      ))}
    </div>
  );
}

export function ContextLedger() {
  const toast = useToast();
  const [events, setEvents] = useState<ContextEvent[]>([]);
  const [selected, setSelected] = useState<ContextEvent | null>(null);
  const [notePath, setNotePath] = useState<string>("");
  const [noteText, setNoteText] = useState<string>("");
  const [focus, setFocus] = useState<string>("");
  const [scanning, setScanning] = useState(false);
  const [rerunning, setRerunning] = useState(false);
  const [query, setQuery] = useState<string>("");
  const [hits, setHits] = useState<ContextHit[]>([]);
  const [searching, setSearching] = useState(false);
  const [evalReport, setEvalReport] = useState<ContextEvalReport | null>(null);
  const [evaluating, setEvaluating] = useState(false);

  const openNote = useCallback(
    async (ev: ContextEvent, overridePath?: string) => {
      const target = overridePath ?? ev.note;
      setSelected(ev);
      setNotePath(target);
      try {
        const text = await api.readNote(target);
        setNoteText(text);
      } catch (e) {
        setNoteText(`Could not read ${target}: ${errorText(e)}`);
      }
    },
    [],
  );

  const load = useCallback(async () => {
    try {
      const rows = await api.contextEvents(200);
      setEvents(rows);
      if (rows.length > 0 && !selected) {
        void openNote(rows[0]);
      }
    } catch (e) {
      toast("error", errorText(e));
    }
  }, [openNote, selected, toast]);

  useEffect(() => {
    void load();
  }, [load]);

  async function runScan() {
    setScanning(true);
    try {
      const r = await api.contextScan();
      toast(
        "ok",
        `Scanned ${r.found} compactions: ${r.recorded} new events, ${r.notes_written} notes written`,
      );
      const rows = await api.contextEvents(200);
      setEvents(rows);
      if (rows.length > 0) {
        void openNote(rows[0]);
      }
    } catch (e) {
      toast("error", errorText(e));
    } finally {
      setScanning(false);
    }
  }

  async function runRerun() {
    if (!selected || !focus.trim()) return;
    setRerunning(true);
    try {
      const rel = await api.contextRerun(selected.id, focus.trim());
      toast("ok", `Wrote focused summary ${rel}`);
      setFocus("");
      await openNote(selected, rel);
    } catch (e) {
      toast("error", errorText(e));
    } finally {
      setRerunning(false);
    }
  }

  async function runSearch() {
    if (!query.trim()) {
      setHits([]);
      return;
    }
    setSearching(true);
    try {
      const res = await api.contextSearch(query.trim(), 20);
      setHits(res);
    } catch (e) {
      toast("error", errorText(e));
    } finally {
      setSearching(false);
    }
  }

  async function runEval() {
    setEvaluating(true);
    try {
      const rep = await api.contextEval(12, 0.8);
      setEvalReport(rep);
      toast(
        "ok",
        `Evaluated ${rep.rows.length} rows across ${rep.sessions} sessions (${rep.studio_passing}/${rep.rows.length} pass 80% bar)`,
      );
    } catch (e) {
      toast("error", errorText(e));
    } finally {
      setEvaluating(false);
    }
  }

  return (
    <div className="flex h-full flex-col overflow-y-auto p-6 text-slate-200">
      <div className="flex flex-wrap items-start justify-between gap-4 border-b border-line pb-4">
        <div>
          <div className="flex items-center gap-2">
            <Archive className="size-5 text-emerald-300" />
            <h1 className="text-lg font-semibold text-slate-100">Context Ledger</h1>
          </div>
          <p className="mt-1 max-w-3xl text-xs text-slate-400">
            Immutable compaction log and 13-section anchored summaries across Hermes and Claude
            Code. Transcripts stay untouched on disk; every compaction writes a permanent note under
            03_Memory/context/ with file, symbol, decision and commit pointers.
          </p>
        </div>
        <div className="flex items-center gap-2">
          <button
            type="button"
            onClick={() => void runScan()}
            disabled={scanning}
            className="inline-flex h-8 items-center gap-1.5 rounded-lg bg-emerald-500/20 px-3 text-xs font-medium text-emerald-200 ring-1 ring-emerald-400/30 hover:bg-emerald-500/30 disabled:opacity-50"
          >
            <RefreshCw className={`size-3.5 ${scanning ? "animate-spin" : ""}`} />
            {scanning ? "Scanning..." : "Scan transcripts"}
          </button>
          <button
            type="button"
            onClick={() => void runEval()}
            disabled={evaluating}
            className="inline-flex h-8 items-center gap-1.5 rounded-lg bg-sky-500/20 px-3 text-xs font-medium text-sky-200 ring-1 ring-sky-400/30 hover:bg-sky-500/30 disabled:opacity-50"
          >
            <FlaskConical className="size-3.5" />
            {evaluating ? "Scoring..." : "Run 4-probe eval"}
          </button>
        </div>
      </div>

      <div className="mt-4 flex flex-wrap items-center gap-2">
        <div className="relative flex-1">
          <Search className="pointer-events-none absolute left-3 top-2.5 size-3.5 text-slate-500" />
          <input
            type="text"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") void runSearch();
            }}
            placeholder="Search Hermes FTS5 transcripts and vault context summaries..."
            className="h-8 w-full rounded-lg bg-white/[0.04] pl-8 pr-3 text-xs text-slate-200 ring-1 ring-white/10 placeholder:text-slate-500 focus:outline-none focus:ring-emerald-400/40"
          />
        </div>
        <button
          type="button"
          onClick={() => void runSearch()}
          disabled={searching}
          className="h-8 rounded-lg bg-white/[0.06] px-3 text-xs font-medium text-slate-200 hover:bg-white/[0.1] disabled:opacity-50"
        >
          {searching ? "Searching..." : "Search"}
        </button>
      </div>

      {hits.length > 0 && (
        <div className="mt-3 rounded-xl border border-line bg-sunken p-3">
          <div className="mb-2 flex items-center justify-between text-xs text-slate-400">
            <span>{hits.length} retrieval hits across transcripts and notes</span>
            <button
              type="button"
              onClick={() => setHits([])}
              className="text-2xs text-slate-500 hover:text-slate-300"
            >
              Clear
            </button>
          </div>
          <div className="max-h-48 space-y-1.5 overflow-y-auto">
            {hits.map((h, i) => (
              <div
                key={`${h.reference}-${i}`}
                className="rounded-lg bg-white/[0.03] px-2.5 py-1.5 text-xs"
              >
                <div className="flex items-center gap-2">
                  <span className="rounded bg-emerald-500/15 px-1.5 py-0.5 font-mono text-2xs text-emerald-300">
                    {h.kind}
                  </span>
                  <span className="font-mono text-2xs text-slate-400">{h.reference}</span>
                </div>
                <p className="mt-1 text-slate-300">{h.text}</p>
              </div>
            ))}
          </div>
        </div>
      )}

      <div className="mt-4 grid min-h-0 flex-1 grid-cols-1 gap-4 lg:grid-cols-12">
        <div className="flex flex-col rounded-xl border border-line bg-sunken p-3 lg:col-span-5">
          <div className="mb-2 flex items-center justify-between">
            <h2 className="text-xs font-semibold uppercase tracking-wider text-slate-400">
              Recorded Compactions ({events.length})
            </h2>
            <span className="text-2xs text-slate-500">Append-only ledger</span>
          </div>
          {events.length === 0 ? (
            <div className="flex flex-1 flex-col items-center justify-center py-12 text-center text-xs text-slate-500">
              <p>No compaction events recorded yet.</p>
              <p className="mt-1">
                Click Scan transcripts to import Hermes and Claude Code compactions.
              </p>
            </div>
          ) : (
            <div className="space-y-2 overflow-y-auto pr-1">
              {events.map((ev) => {
                const active = selected?.id === ev.id;
                return (
                  <button
                    key={ev.id}
                    type="button"
                    onClick={() => void openNote(ev)}
                    className={`w-full rounded-lg p-2.5 text-left transition-colors ${
                      active
                        ? "bg-emerald-500/10 ring-1 ring-emerald-400/30"
                        : "bg-white/[0.03] hover:bg-white/[0.06]"
                    }`}
                  >
                    <div className="flex items-center justify-between gap-2">
                      <span className="rounded bg-white/[0.08] px-1.5 py-0.5 font-mono text-2xs text-emerald-300">
                        {ev.agent}
                      </span>
                      <span className="font-mono text-2xs text-slate-400">
                        msgs #{ev.first_msg}..#{ev.last_msg} ({ev.dropped_msgs} dropped)
                      </span>
                    </div>
                    <div className="mt-1 truncate font-mono text-xs text-slate-200">
                      {ev.session_id}
                    </div>
                    <div className="mt-1 flex flex-wrap items-center gap-2 text-2xs text-slate-400">
                      <span>
                        {fmtTokens(ev.tokens_before)} to {fmtTokens(ev.tokens_after)} tok
                      </span>
                      {ev.model && <span>model: {ev.model}</span>}
                      {ev.commit && <span>commit: {ev.commit.slice(0, 7)}</span>}
                    </div>
                  </button>
                );
              })}
            </div>
          )}
        </div>

        <div className="flex flex-col rounded-xl border border-line bg-sunken p-3 lg:col-span-7">
          {selected ? (
            <>
              <div className="flex flex-wrap items-center justify-between gap-2 border-b border-line pb-2">
                <div className="min-w-0">
                  <div className="flex items-center gap-1.5 text-xs font-medium text-slate-200">
                    <FileText className="size-3.5 text-emerald-300" />
                    <span className="truncate font-mono">{notePath}</span>
                  </div>
                  <div className="mt-0.5 font-mono text-2xs text-slate-500">
                    sha256 {selected.hash.slice(0, 16)} | source {selected.source}
                  </div>
                </div>
              </div>
              <div className="mt-2 flex items-center gap-2">
                <input
                  type="text"
                  value={focus}
                  onChange={(e) => setFocus(e.target.value)}
                  placeholder="Focus hint for a new summary note (original note stays untouched)..."
                  className="h-8 flex-1 rounded-lg bg-white/[0.04] px-2.5 text-xs text-slate-200 ring-1 ring-white/10 placeholder:text-slate-500 focus:outline-none focus:ring-emerald-400/40"
                />
                <button
                  type="button"
                  onClick={() => void runRerun()}
                  disabled={rerunning || !focus.trim()}
                  className="inline-flex h-8 items-center gap-1 rounded-lg bg-emerald-500/20 px-2.5 text-xs font-medium text-emerald-200 hover:bg-emerald-500/30 disabled:opacity-50"
                >
                  <Sparkles className="size-3.5" />
                  {rerunning ? "Writing..." : "Re-summarize"}
                </button>
              </div>
              <pre className="mt-3 max-h-96 flex-1 overflow-y-auto whitespace-pre-wrap rounded-lg bg-black/30 p-3 font-mono text-2xs leading-relaxed text-slate-300">
                {noteText}
              </pre>
            </>
          ) : (
            <div className="flex flex-1 items-center justify-center py-12 text-xs text-slate-500">
              Select a compaction event to inspect its fixed-schema summary note.
            </div>
          )}
        </div>
      </div>

      {evalReport && (
        <div className="mt-4 rounded-xl border border-line bg-sunken p-4">
          <div className="flex flex-wrap items-center justify-between gap-2">
            <div>
              <h2 className="flex items-center gap-1.5 text-sm font-semibold text-slate-100">
                <CheckCircle2 className="size-4 text-emerald-300" />
                Four-Probe Compaction Evaluation
              </h2>
              <p className="mt-0.5 text-xs text-slate-400">
                {evalReport.sessions} real sessions (&gt;= 12 messages), {evalReport.rows.length}{" "}
                evaluated cuts, {evalReport.studio_passing}/{evalReport.rows.length} passing the{" "}
                {fmtScore(evalReport.pass_bar)} probe bar, avg{" "}
                {fmtTokens(evalReport.tokens_per_session)} tokens/session.
              </p>
            </div>
            <div className="flex flex-wrap gap-4 text-xs">
              <div>
                <div className="mb-1 text-2xs uppercase text-slate-400">Baseline mean</div>
                <ScorePills s={evalReport.baseline_mean} bar={evalReport.pass_bar} />
              </div>
              <div>
                <div className="mb-1 text-2xs uppercase text-emerald-300">Studio anchored mean</div>
                <ScorePills s={evalReport.studio_mean} bar={evalReport.pass_bar} />
              </div>
            </div>
          </div>

          <div className="mt-3 max-h-64 overflow-y-auto">
            <table className="w-full text-left text-xs">
              <thead className="border-b border-line text-2xs uppercase text-slate-400">
                <tr>
                  <th className="py-1.5 pr-2">Agent</th>
                  <th className="py-1.5 pr-2">Kind</th>
                  <th className="py-1.5 pr-2">Session</th>
                  <th className="py-1.5 pr-2">Dropped</th>
                  <th className="py-1.5 pr-2">Baseline</th>
                  <th className="py-1.5">Studio anchored</th>
                </tr>
              </thead>
              <tbody className="divide-y divide-white/5">
                {evalReport.rows.map((r, i) => (
                  <tr key={`${r.session}-${r.kind}-${i}`}>
                    <td className="py-1.5 pr-2 font-mono text-2xs text-slate-300">{r.agent}</td>
                    <td className="py-1.5 pr-2 font-mono text-2xs text-slate-400">{r.kind}</td>
                    <td className="py-1.5 pr-2 font-mono text-2xs text-slate-300">
                      {r.session.slice(0, 22)}
                    </td>
                    <td className="py-1.5 pr-2 font-mono text-2xs text-slate-400">
                      {r.dropped} msgs
                    </td>
                    <td className="py-1.5 pr-2">
                      <ScorePills s={r.baseline} bar={evalReport.pass_bar} />
                    </td>
                    <td className="py-1.5">
                      <ScorePills s={r.studio} bar={evalReport.pass_bar} />
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </div>
      )}

      <div className="mt-4 text-right text-2xs text-slate-500">
        AgentOS Studio Context Ledger by K-ONE IT SOLUTIONS, Kone Tshivhinda
      </div>
    </div>
  );
}
