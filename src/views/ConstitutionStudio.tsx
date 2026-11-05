import { BookOpen, FileText, Radio, Save, Search, ScrollText } from "lucide-react";
import { useCallback, useEffect, useState } from "react";
import { api, errorText, type BroadcastReport, type Law, type NoteInfo, type SearchHit } from "../lib/api";
import { relativeTime } from "../lib/format";
import { Button, Card, Toggle } from "../components/ui";
import { useToast } from "../components/Toasts";

const LAW_HINT: Record<number, string> = {
  1: "Zero-Confirmation Autonomy",
  2: "Ban Em-Dashes and Vibecoding",
  3: "Auto-PR and Merge",
  4: "Rust / Fast-Language Preference",
  5: "Mandatory Markdown Sync",
  6: "AgentOS Bus Protocol",
};

export function ConstitutionStudio({ memoryTick }: { memoryTick: number }) {
  const toast = useToast();
  const [text, setText] = useState("");
  const [system, setSystem] = useState("");
  const [laws, setLaws] = useState<Law[]>([]);
  const [path, setPath] = useState("");
  const [tab, setTab] = useState<"constitution" | "system">("constitution");
  const [includeRepos, setIncludeRepos] = useState(true);
  const [saving, setSaving] = useState(false);
  const [report, setReport] = useState<BroadcastReport | null>(null);
  const [dirty, setDirty] = useState(false);

  const load = useCallback(async () => {
    try {
      const c = await api.loadConstitution();
      setText(c.constitution);
      setSystem(c.system_context);
      setLaws(c.laws);
      setPath(c.path);
      setDirty(false);
    } catch (e) {
      toast("error", errorText(e));
    }
  }, [toast]);

  useEffect(() => {
    void load();
  }, [load]);

  async function toggleLaw(n: number, enabled: boolean) {
    try {
      const r = await api.toggleLaw(text, n, enabled);
      setText(r.constitution);
      setLaws(r.laws);
      setDirty(true);
    } catch (e) {
      toast("error", errorText(e));
    }
  }

  async function broadcast() {
    setSaving(true);
    try {
      const r = await api.broadcast(text, system, includeRepos);
      setReport(r);
      setDirty(false);
      const changed = r.repos.reduce((n, x) => n + x.actions.filter(([, a]) => a !== "unchanged").length, 0);
      toast("ok", `Broadcast complete: vault, 3 global rule files${includeRepos ? `, ${r.repos.length} repositories (${changed} files changed)` : ""}${r.dashes_replaced ? ". Dashes were replaced (Law 2)." : ""}`);
      await load();
    } catch (e) {
      toast("error", errorText(e));
    } finally {
      setSaving(false);
    }
  }

  return (
    <div className="grid h-full min-h-0 grid-cols-[minmax(0,1fr)_380px] gap-5 px-6 pb-6 pt-5">
      <div className="flex min-h-0 flex-col gap-4">
        <div className="flex items-end justify-between gap-4">
          <div>
            <h1 className="text-lg font-semibold text-slate-100">Constitution and Shared Memory</h1>
            <p className="max-w-2xl truncate text-xs text-muted" title={path}>
              {path}
            </p>
          </div>
          <div className="flex items-center gap-3">
            <label className="flex items-center gap-2 text-xs text-slate-400">
              <Toggle checked={includeRepos} onChange={setIncludeRepos} label="Include repositories" />
              Include all repositories
            </label>
            <Button variant="primary" busy={saving} icon={<Radio className="size-4" />} onClick={broadcast}>
              Save and Broadcast
            </Button>
          </div>
        </div>

        <Card className="grid grid-cols-3 gap-px overflow-hidden bg-line">
          {laws.map((l) => (
            <div key={l.number} className="flex items-center justify-between gap-3 bg-card px-4 py-3">
              <div className="min-w-0">
                <div className="truncate text-sm font-medium text-slate-100">{LAW_HINT[l.number] ?? l.title}</div>
                <div className="truncate text-[11px] text-muted">
                  Law {l.number}: {l.title}
                </div>
              </div>
              <Toggle checked={l.enabled} onChange={(v) => toggleLaw(l.number, v)} label={`Law ${l.number}`} />
            </div>
          ))}
        </Card>

        <Card className="flex min-h-0 flex-1 flex-col overflow-hidden">
          <div className="flex items-center justify-between border-b border-line px-3 pt-2">
            <div className="flex gap-1">
              {(
                [
                  ["constitution", "CONSTITUTION.md"],
                  ["system", "SYSTEM_CONTEXT.md"],
                ] as const
              ).map(([id, label]) => (
                <button
                  key={id}
                  onClick={() => setTab(id)}
                  className={`-mb-px flex items-center gap-1.5 border-b-2 px-3 py-2 text-xs font-medium ${tab === id ? "border-emerald-400 text-slate-100" : "border-transparent text-slate-500 hover:text-slate-300"}`}
                >
                  <ScrollText className="size-3.5" />
                  {label}
                </button>
              ))}
            </div>
            {dirty && <span className="pb-2 text-[11px] text-amber-300">Unsaved changes</span>}
          </div>
          <textarea
            spellCheck={false}
            value={tab === "constitution" ? text : system}
            onChange={(e) => {
              setDirty(true);
              if (tab === "constitution") setText(e.target.value);
              else setSystem(e.target.value);
            }}
            className="min-h-0 flex-1 resize-none bg-[#0a0e16] p-4 font-mono text-[12.5px] leading-relaxed text-slate-200 outline-none"
          />
        </Card>
        {report && (
          <div className="text-[11px] text-muted">
            Last broadcast: {report.vault.length} vault files, {report.global.map(([p, a]) => `${p.split(/[\\/]/).slice(-2).join("/")} ${a}`).join(", ")}
            {report.repos.length > 0 && `, ${report.repos.length} repositories`}
          </div>
        )}
      </div>
      <MemoryPanel tick={memoryTick} />
    </div>
  );
}

function MemoryPanel({ tick }: { tick: number }) {
  const toast = useToast();
  const [notes, setNotes] = useState<NoteInfo[]>([]);
  const [query, setQuery] = useState("");
  const [hits, setHits] = useState<SearchHit[] | null>(null);
  const [open, setOpen] = useState<{ path: string; text: string } | null>(null);

  useEffect(() => {
    api.memoryNotes().then(setNotes).catch((e) => toast("error", errorText(e)));
  }, [tick, toast]);

  async function search(q: string) {
    setQuery(q);
    if (!q.trim()) {
      setHits(null);
      return;
    }
    try {
      setHits(await api.searchMemory(q));
    } catch (e) {
      toast("error", errorText(e));
    }
  }

  async function read(path: string) {
    try {
      setOpen({ path, text: await api.readNote(path) });
    } catch (e) {
      toast("error", errorText(e));
    }
  }

  return (
    <Card className="flex min-h-0 flex-col overflow-hidden">
      <div className="border-b border-line p-3">
        <div className="mb-2 flex items-center gap-2 text-sm font-semibold text-slate-100">
          <BookOpen className="size-4 text-emerald-300" /> Shared Memory
        </div>
        <div className="flex items-center gap-2 rounded-lg border border-line bg-canvas px-2.5">
          <Search className="size-3.5 text-slate-500" />
          <input value={query} onChange={(e) => void search(e.target.value)} placeholder="Search 01_Projects and 03_Memory" className="h-8 flex-1 bg-transparent text-xs outline-none placeholder:text-slate-600" />
        </div>
      </div>
      {open ? (
        <div className="flex min-h-0 flex-1 flex-col">
          <div className="flex items-center justify-between border-b border-line px-3 py-2">
            <span className="truncate font-mono text-[11px] text-slate-400">{open.path}</span>
            <Button size="sm" variant="ghost" onClick={() => setOpen(null)}>
              Back
            </Button>
          </div>
          <pre className="min-h-0 flex-1 overflow-auto whitespace-pre-wrap break-words p-3 font-mono text-[11.5px] leading-relaxed text-slate-300">{open.text}</pre>
        </div>
      ) : (
        <ul className="min-h-0 flex-1 divide-y divide-line overflow-y-auto">
          {hits
            ? hits.map((h) => (
                <li key={h.path}>
                  <button onClick={() => read(h.relative_path)} className="block w-full px-3 py-2.5 text-left hover:bg-white/[0.03]">
                    <div className="truncate text-xs font-medium text-slate-200">{h.relative_path}</div>
                    {h.matches.slice(0, 2).map((m) => (
                      <div key={m.line} className="truncate text-[11px] text-muted">
                        L{m.line}: {m.snippet}
                      </div>
                    ))}
                  </button>
                </li>
              ))
            : notes.map((n) => (
                <li key={n.relative_path}>
                  <button onClick={() => read(n.relative_path)} className="flex w-full items-center gap-2 px-3 py-2.5 text-left hover:bg-white/[0.03]">
                    <FileText className="size-3.5 shrink-0 text-slate-500" />
                    <div className="min-w-0 flex-1">
                      <div className="truncate text-xs font-medium text-slate-200">{n.title}</div>
                      <div className="text-[11px] text-slate-500">{relativeTime(n.modified)}</div>
                    </div>
                    {n.relative_path.endsWith("DECISION_LOG.md") && <Save className="size-3.5 text-violet-300" />}
                  </button>
                </li>
              ))}
          {hits && hits.length === 0 && <li className="px-3 py-6 text-center text-xs text-slate-500">No matches</li>}
        </ul>
      )}
    </Card>
  );
}
