import { useCallback, useEffect, useMemo, useState, type ReactNode } from "react";
import {
  CheckCircle2,
  CircleAlert,
  ClipboardCopy,
  FilePlus2,
  FolderInput,
  History,
  Info,
  MessageCircleQuestion,
  RefreshCw,
  Save,
  Search,
  Send,
  Sparkles,
  Trash2,
  Wand2,
} from "lucide-react";
import {
  api,
  errorText,
  type Project,
  type PromptDetail,
  type PromptFinding,
  type PromptSession,
  type SavedPrompt,
} from "../lib/api";
import { Button, Card, Empty, inputClass } from "../components/ui";
import { useToast } from "../components/Toasts";

const KINDS: { id: SavedPrompt["kind"]; label: string; hint: string }[] = [
  { id: "continue", label: "Continue the project", hint: "Pick up the next open task or phase" },
  { id: "new_session", label: "Hand over a session", hint: "Carry a Hermes session's work into a new one" },
  { id: "review", label: "Review a change", hint: "A strict second engineer on a PR or branch" },
  { id: "fix", label: "Fix a problem", hint: "Root cause first, then a test, then the fix" },
  { id: "custom", label: "Anything else", hint: "Your words, with the project context added" },
];

const TARGETS: { id: string; label: string }[] = [
  { id: "claude-code", label: "Claude Code" },
  { id: "antigravity", label: "Antigravity" },
  { id: "qoder", label: "Qoder" },
  { id: "codex", label: "Codex CLI" },
  { id: "cursor", label: "Cursor" },
  { id: "claude-desktop", label: "Claude Desktop" },
  { id: "chat", label: "A chat model (no tools)" },
];

const BUS_AGENTS = ["hermes", "claude-code", "antigravity", "qoder"];

function blank(project: string): SavedPrompt {
  return {
    id: 0,
    title: "",
    project,
    kind: "continue",
    target: "claude-code",
    body: "",
    version: 0,
    uses: 0,
    created_at: 0,
    updated_at: 0,
  };
}

function when(at: number): string {
  return at ? new Date(at * 1000).toLocaleString() : "";
}

function tokens(text: string): number {
  return Math.ceil(text.length / 4);
}

const LEVEL_ICON: Record<PromptFinding["level"], ReactNode> = {
  error: <CircleAlert className="mt-0.5 size-4 shrink-0 text-red-300" />,
  warn: <CircleAlert className="mt-0.5 size-4 shrink-0 text-warn" />,
  info: <Info className="mt-0.5 size-4 shrink-0 text-sky-300" />,
};

/**
 * The prompt workbench (docs/PROMPTS.md): write a prompt from what Studio knows about a project,
 * keep every version, check it, sharpen it, and hand it to another agent or app.
 */
export function Prompts() {
  const toast = useToast();
  const [projects, setProjects] = useState<Project[]>([]);
  const [project, setProject] = useState("");
  const [query, setQuery] = useState("");
  const [list, setList] = useState<SavedPrompt[]>([]);
  const [draft, setDraft] = useState<SavedPrompt>(blank(""));
  const [detail, setDetail] = useState<PromptDetail | null>(null);
  const [ask, setAsk] = useState("");
  const [sessions, setSessions] = useState<PromptSession[]>([]);
  const [session, setSession] = useState("");
  const [findings, setFindings] = useState<PromptFinding[] | null>(null);
  const [questions, setQuestions] = useState("");
  const [busy, setBusy] = useState("");

  const loadList = useCallback(async () => {
    try {
      setList(await api.promptsList(project || undefined, query || undefined));
    } catch (e) {
      toast("error", errorText(e));
    }
  }, [project, query, toast]);

  useEffect(() => {
    void api
      .projects()
      .then((ps) => setProjects(ps.filter((p) => !p.archived)))
      .catch(() => setProjects([]));
  }, []);
  useEffect(() => {
    void loadList();
  }, [loadList]);
  useEffect(() => {
    if (draft.kind === "new_session" && sessions.length === 0) {
      void api.promptSessions().then(setSessions).catch(() => setSessions([]));
    }
  }, [draft.kind, sessions.length]);

  const dirty = useMemo(() => {
    const saved = list.find((p) => p.id === draft.id);
    return !saved || saved.body !== draft.body || saved.title !== draft.title || saved.target !== draft.target;
  }, [list, draft]);

  async function run<T>(label: string, f: () => Promise<T>): Promise<T | undefined> {
    setBusy(label);
    try {
      return await f();
    } catch (e) {
      toast("error", errorText(e));
      return undefined;
    } finally {
      setBusy("");
    }
  }

  async function open(id: number) {
    const d = await run("open", () => api.promptGet(id));
    if (d) {
      setDraft(d.prompt);
      setDetail(d);
      setFindings(null);
      setQuestions("");
    }
  }

  function startNew() {
    setDraft(blank(project));
    setDetail(null);
    setFindings(null);
    setQuestions("");
    setAsk("");
  }

  async function compose() {
    if (!draft.project) {
      toast("error", "Choose the project the prompt is about first.");
      return;
    }
    const body = await run("compose", () =>
      api.promptCompose(draft.project, draft.kind, draft.target, ask, draft.kind === "new_session" ? session : undefined),
    );
    if (body !== undefined) {
      const title = draft.title || (body.split("\n")[0] ?? "").replace(/^#\s*/, "").slice(0, 80);
      setDraft({ ...draft, body, title });
      setFindings(null);
    }
  }

  async function refresh() {
    const body = await run("refresh", () => api.promptRefresh(draft.project, draft.body));
    if (body !== undefined) {
      setDraft({ ...draft, body });
      toast("ok", "Context refreshed with today's commits, tasks and missions.");
    }
  }

  async function check() {
    const f = await run("check", () => api.promptLint(draft.project, draft.target, draft.body));
    if (f) setFindings(f);
  }

  async function sharpen() {
    const body = await run("sharpen", () => api.promptAi(draft.project, draft.target, "sharpen", draft.body));
    if (body) {
      setDraft({ ...draft, body });
      toast("info", "Sharpened. Save keeps the old wording as an earlier version.");
    }
  }

  async function unclear() {
    const q = await run("questions", () => api.promptAi(draft.project, draft.target, "questions", draft.body));
    if (q !== undefined) setQuestions(q);
  }

  async function save(note = ""): Promise<SavedPrompt | undefined> {
    const saved = await run("save", () => api.promptSave(draft, note));
    if (saved) {
      setDraft(saved);
      await loadList();
      const d = await api.promptGet(saved.id).catch(() => null);
      setDetail(d);
    }
    return saved;
  }

  async function deliver(via: string) {
    const saved = dirty || draft.id === 0 ? await save(`before ${via}`) : draft;
    if (!saved) return;
    const out = await run("deliver", () => api.promptDeliver(saved.id, via));
    if (out === undefined) return;
    if (via === "clipboard") {
      await navigator.clipboard.writeText(out);
      toast("ok", "Copied, with secrets blanked and dashes made hyphens. Paste it into the other app.");
    } else if (via === "file") {
      toast("ok", `Written to ${out}. An agent opened in that folder can read it.`);
    } else {
      toast("ok", `On the bus as ${out} for ${via.slice(4)}.`);
    }
    await loadList();
    setDetail(await api.promptGet(saved.id).catch(() => null));
  }

  async function remove() {
    if (!draft.id) return;
    await run("delete", () => api.promptDelete(draft.id));
    startNew();
    await loadList();
  }

  const projectName = (slug: string) => projects.find((p) => p.slug === slug)?.name ?? slug;

  return (
    <div className="flex h-full min-h-0 gap-4 p-5">
      <Card className="flex w-80 shrink-0 flex-col overflow-hidden">
        <div className="space-y-2 border-b border-line p-3">
          <div className="flex items-center justify-between">
            <div className="text-small font-semibold text-slate-200">Saved prompts</div>
            <Button size="sm" icon={<FilePlus2 className="size-3.5" />} onClick={startNew}>
              New
            </Button>
          </div>
          <select className={inputClass} value={project} onChange={(e) => setProject(e.target.value)} aria-label="Project filter">
            <option value="">Every project</option>
            {projects.map((p) => (
              <option key={p.slug} value={p.slug}>
                {p.name}
              </option>
            ))}
          </select>
          <div className="relative">
            <Search className="pointer-events-none absolute left-2.5 top-2.5 size-4 text-slate-500" />
            <input className={`${inputClass} pl-8`} placeholder="Search titles and text" value={query} onChange={(e) => setQuery(e.target.value)} />
          </div>
        </div>
        <div className="min-h-0 flex-1 overflow-y-auto p-2">
          {list.length === 0 ? (
            <Empty icon={<Wand2 className="size-5" />} title="No saved prompts" hint="Write one from a project on the right; every version is kept." />
          ) : (
            list.map((p) => (
              <button
                key={p.id}
                onClick={() => void open(p.id)}
                className={`mb-1 w-full rounded-lg px-3 py-2 text-left transition-colors hover:bg-white/[0.04] ${p.id === draft.id ? "bg-white/[0.06] ring-1 ring-line" : ""}`}
              >
                <div className="truncate text-small text-slate-200">{p.title}</div>
                <div className="text-2xs text-muted">
                  {projectName(p.project)} . {TARGETS.find((t) => t.id === p.target)?.label ?? p.target} . v{p.version}
                  {p.uses > 0 && ` . sent ${p.uses}x`}
                </div>
              </button>
            ))
          )}
        </div>
      </Card>

      <div className="flex min-w-0 flex-1 flex-col gap-4 overflow-y-auto">
        <Card className="space-y-3 p-4">
          <div className="grid grid-cols-1 gap-3 md:grid-cols-3">
            <label className="space-y-1">
              <span className="text-label text-muted">Project</span>
              <select className={inputClass} value={draft.project} onChange={(e) => setDraft({ ...draft, project: e.target.value })}>
                <option value="">Choose a project</option>
                {projects.map((p) => (
                  <option key={p.slug} value={p.slug}>
                    {p.name}
                  </option>
                ))}
              </select>
            </label>
            <label className="space-y-1">
              <span className="text-label text-muted">What it is for</span>
              <select className={inputClass} value={draft.kind} onChange={(e) => setDraft({ ...draft, kind: e.target.value as SavedPrompt["kind"] })}>
                {KINDS.map((k) => (
                  <option key={k.id} value={k.id} title={k.hint}>
                    {k.label}
                  </option>
                ))}
              </select>
            </label>
            <label className="space-y-1">
              <span className="text-label text-muted">Who receives it</span>
              <select className={inputClass} value={draft.target} onChange={(e) => setDraft({ ...draft, target: e.target.value })}>
                {TARGETS.map((t) => (
                  <option key={t.id} value={t.id}>
                    {t.label}
                  </option>
                ))}
              </select>
            </label>
          </div>
          {draft.kind === "new_session" && (
            <label className="block space-y-1">
              <span className="text-label text-muted">Session to hand over</span>
              <select className={inputClass} value={session} onChange={(e) => setSession(e.target.value)}>
                <option value="">Choose a Hermes session</option>
                {sessions.map((s) => (
                  <option key={s.id} value={s.id}>
                    {s.title || s.id.slice(0, 20)} ({s.messages} messages, {when(s.at)})
                  </option>
                ))}
              </select>
            </label>
          )}
          <label className="block space-y-1">
            <span className="text-label text-muted">In your own words (optional)</span>
            <textarea
              className={`${inputClass} h-16 py-2`}
              placeholder="Finish phase 3 and release it; or: the login page crashes on an empty password"
              value={ask}
              onChange={(e) => setAsk(e.target.value)}
            />
          </label>
          <div className="flex flex-wrap gap-2">
            <Button variant="primary" busy={busy === "compose"} icon={<Sparkles className="size-4" />} onClick={() => void compose()}>
              Draft from the project
            </Button>
            <span className="self-center text-label text-muted">
              Reads the repo, open tasks and phases, missions, PRs, AGENTS.md and the project note{draft.kind === "new_session" ? ", plus the session" : ""}.
            </span>
          </div>
        </Card>

        <Card className="flex flex-col p-4">
          <div className="mb-2 flex flex-wrap items-center gap-2">
            <input
              className={`${inputClass} min-w-0 flex-1`}
              placeholder="Title"
              value={draft.title}
              onChange={(e) => setDraft({ ...draft, title: e.target.value })}
            />
            <span className="text-2xs text-muted">
              about {tokens(draft.body)} tokens{draft.id ? ` . v${draft.version}` : ""}
              {dirty && draft.body ? " . unsaved" : ""}
            </span>
          </div>
          <textarea
            className={`${inputClass} h-[28rem] py-2 font-mono text-xs leading-relaxed`}
            spellCheck={false}
            placeholder="Draft from the project, or write the prompt here."
            value={draft.body}
            onChange={(e) => setDraft({ ...draft, body: e.target.value })}
          />
          <div className="mt-3 flex flex-wrap gap-2">
            <Button busy={busy === "refresh"} disabled={!draft.body || !draft.project} icon={<RefreshCw className="size-4" />} onClick={() => void refresh()}>
              Refresh context
            </Button>
            <Button busy={busy === "check"} disabled={!draft.body} icon={<CheckCircle2 className="size-4" />} onClick={() => void check()}>
              Check
            </Button>
            <Button busy={busy === "sharpen"} disabled={!draft.body} icon={<Wand2 className="size-4" />} onClick={() => void sharpen()}>
              Sharpen
            </Button>
            <Button busy={busy === "questions"} disabled={!draft.body} icon={<MessageCircleQuestion className="size-4" />} onClick={() => void unclear()}>
              What is unclear?
            </Button>
            <Button variant="primary" busy={busy === "save"} disabled={!draft.body || !draft.title} icon={<Save className="size-4" />} onClick={() => void save()}>
              Save
            </Button>
            {draft.id > 0 && (
              <Button variant="ghost" busy={busy === "delete"} icon={<Trash2 className="size-4" />} onClick={() => void remove()}>
                Delete
              </Button>
            )}
          </div>
          {findings && (
            <ul className="mt-3 space-y-1.5 rounded-lg bg-white/[0.02] p-3 ring-1 ring-line">
              {findings.length === 0 ? (
                <li className="flex items-center gap-2 text-small text-emerald-300">
                  <CheckCircle2 className="size-4" /> It has a goal, a finish line, constraints and a report format, and no secrets.
                </li>
              ) : (
                findings.map((f, i) => (
                  <li key={i} className="flex items-start gap-2 text-small text-slate-300">
                    {LEVEL_ICON[f.level]}
                    {f.text}
                  </li>
                ))
              )}
            </ul>
          )}
          {questions && (
            <div className="mt-3 rounded-lg bg-sky-500/[0.05] p-3 text-small text-slate-300 ring-1 ring-sky-500/20">
              <div className="mb-1 font-semibold text-sky-200">What the receiver would ask</div>
              <pre className="whitespace-pre-wrap font-sans">{questions}</pre>
            </div>
          )}
        </Card>

        <Card className="p-4">
          <div className="mb-2 flex items-center gap-2 text-small font-semibold text-slate-200">
            <Send className="size-4" /> Hand it over
          </div>
          <div className="flex flex-wrap gap-2">
            <Button disabled={!draft.body || !draft.title} busy={busy === "deliver"} icon={<ClipboardCopy className="size-4" />} onClick={() => void deliver("clipboard")}>
              Copy for another app
            </Button>
            <Button disabled={!draft.body || !draft.title || !draft.project} icon={<FolderInput className="size-4" />} onClick={() => void deliver("file")}>
              Write into the project
            </Button>
            {BUS_AGENTS.map((a) => (
              <Button key={a} variant="ghost" disabled={!draft.body || !draft.title} icon={<Send className="size-3.5" />} onClick={() => void deliver(`bus:${a}`)}>
                {a}
              </Button>
            ))}
          </div>
          <p className="mt-2 text-label text-muted">
            Sending saves first, blanks secrets and turns dashes into hyphens. Write into the project puts it at
            .agentos/handoff/ in the repo; an agent name puts it on the Agent Bus as a task for that agent.
          </p>
        </Card>

        {detail && (detail.versions.length > 1 || detail.handoffs.length > 0) && (
          <Card className="p-4">
            <div className="mb-2 flex items-center gap-2 text-small font-semibold text-slate-200">
              <History className="size-4" /> History
            </div>
            <div className="grid gap-4 md:grid-cols-2">
              <ul className="space-y-1">
                {detail.versions.map((v) => (
                  <li key={v.version} className="flex items-center justify-between gap-2 text-small">
                    <span className="text-slate-300">
                      v{v.version} <span className="text-2xs text-muted">{when(v.at)} {v.note}</span>
                    </span>
                    {v.version !== draft.version && (
                      <Button size="sm" variant="ghost" onClick={() => setDraft({ ...draft, body: v.body })}>
                        Load
                      </Button>
                    )}
                  </li>
                ))}
              </ul>
              <ul className="space-y-1">
                {detail.handoffs.map((h) => (
                  <li key={h.id} className="text-small text-slate-300">
                    v{h.version} by {h.via} <span className="text-2xs text-muted">{when(h.at)} {h.detail}</span>
                  </li>
                ))}
              </ul>
            </div>
          </Card>
        )}
      </div>
    </div>
  );
}
