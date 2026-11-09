import { useState, type FormEvent, type ReactNode } from "react";
import { GitCommitHorizontal, GitPullRequest, Loader2, Search, Terminal } from "lucide-react";
import { api, errorText, type Provenance } from "../lib/api";
import { goTo } from "../lib/nav";
import { ExternalLink, Field, inputClass } from "./ui";
import { useToast } from "./Toasts";

function when(secs: number) {
  return secs > 0 ? new Date(secs * 1000).toLocaleString() : "";
}

/** Ask the timeline view to select a mission; App switches screens on the nav event. */
export function openMissionTimeline(mission: string) {
  window.dispatchEvent(new CustomEvent<string>("agentos:timeline", { detail: mission }));
  goTo("timeline");
}

function Chip({ children }: { children: ReactNode }) {
  return (
    <span className="rounded-full bg-white/[0.04] px-1.5 py-0.5 text-2xs text-slate-300 ring-1 ring-line">
      {children}
    </span>
  );
}

/** One answer: the blamed commit, its trailer block, and the mission behind it. */
export function ProvenanceCard({ p }: { p: Provenance }) {
  const m = p.mission;
  const trail = p.trailers;
  return (
    <div className="space-y-3 text-small">
      <div className="flex flex-wrap items-center gap-2 text-slate-500">
        <GitCommitHorizontal className="size-3.5 text-sky-300" />
        <span className="font-mono text-slate-300">{p.short}</span>
        <span className="min-w-0 truncate text-slate-200" title={p.subject}>
          {p.subject || "(no subject)"}
        </span>
        <span className="ml-auto shrink-0 text-caption">{when(p.committed_at)}</span>
      </div>

      <div className="flex flex-wrap items-center gap-1.5">
        <Chip>
          {p.file}:{p.line}
        </Chip>
        <Chip>found by {p.source}</Chip>
        {trail.model && <Chip>model {trail.model}</Chip>}
        {trail.cost && <Chip>cost {trail.cost}</Chip>}
      </div>

      {m ? (
        <div className="rounded-lg border border-line bg-canvas/60 p-3">
          <div className="flex flex-wrap items-center gap-2">
            <span className="font-mono text-caption text-slate-400">{m.id}</span>
            <button
              onClick={() => openMissionTimeline(m.id)}
              className="truncate text-left text-small font-medium text-slate-200 hover:text-sky-300"
              title="Open this mission on the run timeline"
            >
              {m.goal}
            </button>
            <span className="ml-auto shrink-0 rounded-full bg-white/[0.04] px-1.5 py-0.5 text-2xs text-slate-400 ring-1 ring-line">
              {m.status}
            </span>
          </div>
          <div className="mt-2 grid grid-cols-2 gap-x-4 gap-y-1 text-caption text-slate-500">
            <div>
              agent <span className="text-slate-300">{m.agent}</span>
            </div>
            <div>
              model <span className="text-slate-300">{m.model}</span>
            </div>
            <div>
              reviewer <span className="text-slate-300">{m.reviewer ?? "none"}</span>
            </div>
            <div>
              cost <span className="text-slate-300">{m.cost}</span>
            </div>
          </div>
          <div className="mt-2 flex flex-wrap items-center gap-2">
            {m.pr_url && (
              <ExternalLink href={m.pr_url} className="text-caption">
                pull request
              </ExternalLink>
            )}
            {m.sessions.map((s) => (
              <span key={s} className="inline-flex items-center gap-1 font-mono text-2xs text-slate-500" title="Hermes session">
                <Terminal className="size-3" />
                {s}
              </span>
            ))}
          </div>
        </div>
      ) : (
        <div className="rounded-lg border border-line bg-canvas/60 p-3 text-caption text-slate-500">
          No mission names this commit. It is a human or pre-trailer commit
          {p.pull_request ? " from a pull request with no matching mission" : ""}. The trailer block is written when a mission merges through Studio.
        </div>
      )}

      {!m && p.note && <div className="text-caption text-amber-200/80">{p.note}</div>}

      {p.pull_request && (
        <div className="flex items-center gap-2 text-caption text-slate-500">
          <GitPullRequest className="size-3.5 text-emerald-300" />
          <ExternalLink href={p.pull_request} className="truncate">
            {p.pull_request}
          </ExternalLink>
        </div>
      )}
    </div>
  );
}

/** Brain panel: name a repository, a file and a line, and read back the mission that put it there. */
export function WhyPanel() {
  const toast = useToast();
  const [repo, setRepo] = useState("");
  const [file, setFile] = useState("");
  const [line, setLine] = useState("1");
  const [busy, setBusy] = useState(false);
  const [result, setResult] = useState<Provenance | null>(null);

  async function ask(e: FormEvent) {
    e.preventDefault();
    if (!repo.trim() || !file.trim()) {
      toast("error", "Name the repository and the file.");
      return;
    }
    const n = Number(line);
    if (!Number.isInteger(n) || n < 1) {
      toast("error", "The line must be a whole number starting at 1.");
      return;
    }
    setBusy(true);
    try {
      setResult(await api.fileProvenance(repo.trim(), file.trim(), n));
    } catch (err) {
      setResult(null);
      toast("error", errorText(err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="max-h-80 shrink-0 overflow-y-auto border-t border-line">
      <div className="flex items-center gap-2 border-b border-line px-4 py-2 text-small font-medium text-slate-400">
        <GitCommitHorizontal className="size-3.5 text-sky-300" />
        Why is this here?
        <span className="text-caption font-normal text-slate-500">blame a line back to its mission</span>
      </div>
      <form onSubmit={(e) => void ask(e)} className="grid grid-cols-[1fr_1fr_4rem_auto] gap-2 px-4 py-2">
        <Field label="Repository">
          <input
            value={repo}
            onChange={(e) => setRepo(e.target.value)}
            placeholder="project slug or clone folder"
            className={inputClass}
            spellCheck={false}
          />
        </Field>
        <Field label="File">
          <input
            value={file}
            onChange={(e) => setFile(e.target.value)}
            placeholder="src/main.rs"
            className={inputClass}
            spellCheck={false}
          />
        </Field>
        <Field label="Line">
          <input
            value={line}
            onChange={(e) => setLine(e.target.value)}
            inputMode="numeric"
            className={inputClass}
          />
        </Field>
        <button
          type="submit"
          disabled={busy}
          className="mt-5 inline-flex h-8 items-center gap-1.5 self-start rounded-lg px-2.5 text-xs text-slate-300 ring-1 ring-line hover:bg-white/[0.04] disabled:opacity-50"
          title="git blame the line, then read its mission trailers or merged pull request"
        >
          {busy ? <Loader2 className="size-3.5 animate-spin" /> : <Search className="size-3.5" />}
          Blame
        </button>
      </form>
      {result && (
        <div className="border-t border-line/60 px-4 py-3">
          <ProvenanceCard p={result} />
        </div>
      )}
    </div>
  );
}
