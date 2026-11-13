import hljs from "highlight.js/lib/common";
import {
  ChevronDown,
  ChevronRight,
  Code2,
  Copy,
  Eye,
  File,
  FileCode2,
  FileDiff,
  FileText,
  Folder,
  FolderGit2,
  FolderOpen,
  GitBranch,
  Loader2,
  MessageSquarePlus,
  RefreshCw,
  Search,
  SquareArrowOutUpRight,
} from "lucide-react";
import { memo, useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { Markdown } from "../components/Markdown";
import { useToast } from "../components/Toasts";
import { Badge, Button, Empty, inputClass } from "../components/ui";
import { errorText, type RepoInfo } from "../lib/api";
import { explorer, type ExplorerChange, type ExplorerCheckout, type ExplorerFile, type ExplorerFileList, type ExplorerHit } from "../lib/explorer";
import {
  ancestors,
  buildTree,
  checkoutLabel,
  diffStat,
  escapeHtml,
  excerpt,
  fileSize,
  languageFor,
  parseDiff,
  quickOpen,
  splitHighlighted,
  type TreeNode,
} from "../lib/explorerModel";
import { studio } from "../lib/studio";

export type ExploreIntent = { root: string; path?: string; nonce: number };

type Side = "files" | "changes" | "search";
type Mode = "code" | "diff" | "preview";
type Sel = { from: number; to: number } | null;

/** Above this many lines the viewer shows plain text: highlighting would stall the window. */
const HIGHLIGHT_MAX_LINES = 8000;
const POLL_MS = 10_000;

const KIND_STYLE: Record<ExplorerChange["kind"], { letter: string; cls: string }> = {
  modified: { letter: "M", cls: "text-amber-300" },
  added: { letter: "A", cls: "text-emerald-300" },
  untracked: { letter: "U", cls: "text-emerald-300" },
  deleted: { letter: "D", cls: "text-red-300" },
  renamed: { letter: "R", cls: "text-sky-300" },
  copied: { letter: "C", cls: "text-sky-300" },
  conflict: { letter: "!", cls: "text-red-400" },
};

const norm = (p: string) => p.replace(/\\/g, "/").replace(/\/+$/, "").toLowerCase();

function stored(key: string): string {
  try {
    return localStorage.getItem(key) ?? "";
  } catch {
    return "";
  }
}

function FileIcon({ path }: { path: string }) {
  const lang = languageFor(path);
  if (lang === "markdown") return <FileText className="size-3.5 shrink-0 text-slate-500" />;
  if (lang) return <FileCode2 className="size-3.5 shrink-0 text-slate-500" />;
  return <File className="size-3.5 shrink-0 text-slate-600" />;
}

export function Explorer({
  repos,
  intent,
  active,
  onAskHermes,
}: {
  repos: RepoInfo[];
  intent: ExploreIntent | null;
  active: boolean;
  onAskHermes: (prompt: string, cwd?: string) => void;
}) {
  const toast = useToast();
  const [repo, setRepo] = useState<string>(() => stored("explorer-repo"));
  const [checkouts, setCheckouts] = useState<ExplorerCheckout[]>([]);
  const [root, setRoot] = useState<string>("");
  const [list, setList] = useState<ExplorerFileList | null>(null);
  const [loadErr, setLoadErr] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [changes, setChanges] = useState<ExplorerChange[]>([]);
  const [side, setSide] = useState<Side>("files");
  const [filter, setFilter] = useState("");
  const [open, setOpen] = useState<Set<string>>(new Set());
  const [file, setFile] = useState<string | null>(null);
  const [view, setView] = useState<ExplorerFile | null>(null);
  const [diff, setDiff] = useState<string | null>(null);
  const [mode, setMode] = useState<Mode>("code");
  const [fileErr, setFileErr] = useState<string | null>(null);
  const [sel, setSel] = useState<Sel>(null);
  const [jump, setJump] = useState<{ line: number; nonce: number } | null>(null);
  const [query, setQuery] = useState("");
  const [caseSensitive, setCaseSensitive] = useState(false);
  const [hits, setHits] = useState<ExplorerHit[] | null>(null);
  const [searching, setSearching] = useState(false);
  const filterRef = useRef<HTMLInputElement>(null);
  const searchRef = useRef<HTMLInputElement>(null);
  const pendingFile = useRef<string | null>(null);
  const statusSig = useRef("");

  // Repositories the fleet scan found, plus a folder opened from elsewhere (a chat's folder).
  const options = useMemo(() => {
    const seen = new Set<string>();
    const out: { name: string; path: string }[] = [];
    for (const r of repos) {
      if (seen.has(norm(r.path))) continue;
      seen.add(norm(r.path));
      out.push({ name: r.name, path: r.path });
    }
    out.sort((a, b) => a.name.localeCompare(b.name, undefined, { sensitivity: "base" }));
    if (repo && !seen.has(norm(repo))) out.unshift({ name: repo.split(/[\\/]/).pop() || repo, path: repo });
    return out;
  }, [repos, repo]);

  // Default to the first repository once the scan arrives.
  useEffect(() => {
    if (!repo && options.length) setRepo(options[0].path);
  }, [repo, options]);

  useEffect(() => {
    if (repo) localStorage.setItem("explorer-repo", repo);
  }, [repo]);

  // Another screen asked to show a folder (and maybe a file).
  useEffect(() => {
    if (!intent) return;
    const same = checkouts.find((c) => norm(c.path) === norm(intent.root));
    if (same || (repo && norm(intent.root) === norm(repo))) {
      // Already this repository (or one of its worktrees): switch checkout and open the file.
      const target = same?.path ?? repo;
      if (norm(target) === norm(root)) {
        if (intent.path) openFile(intent.path);
      } else {
        pendingFile.current = intent.path ?? null;
        setRoot(target);
      }
      return;
    }
    pendingFile.current = intent.path ?? null;
    setRepo(intent.root);
    setRoot("");
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [intent?.nonce]);

  // A repository's checkouts: itself and the worktrees agents work in.
  useEffect(() => {
    if (!repo) return;
    let stop = false;
    setCheckouts([]);
    explorer
      .checkouts(repo)
      .then((c) => {
        if (stop) return;
        setCheckouts(c);
        setRoot((cur) => (cur && c.some((x) => norm(x.path) === norm(cur)) ? cur : repo));
      })
      .catch((e) => {
        if (stop) return;
        setCheckouts([]);
        setRoot(repo);
        setLoadErr(errorText(e));
      });
    return () => {
      stop = true;
    };
  }, [repo]);

  const openFile = useCallback((path: string, opts: { line?: number; mode?: Mode } = {}) => {
    setFile(path);
    setMode(opts.mode ?? "code");
    setSel(opts.line ? { from: opts.line, to: opts.line } : null);
    setJump(opts.line ? { line: opts.line, nonce: Date.now() } : null);
    setOpen((s) => {
      const next = new Set(s);
      ancestors(path).forEach((a) => next.add(a));
      return next;
    });
  }, []);

  const loadAll = useCallback(async () => {
    if (!root) return;
    setLoading(true);
    setLoadErr(null);
    try {
      const [l, st] = await Promise.all([explorer.files(root), explorer.status(root).catch(() => [] as ExplorerChange[])]);
      setList(l);
      setChanges(st);
      statusSig.current = st.map((c) => c.code + c.path).join("|");
      const want = pendingFile.current;
      pendingFile.current = null;
      if (want) openFile(want);
    } catch (e) {
      setList(null);
      setChanges([]);
      setLoadErr(errorText(e));
    } finally {
      setLoading(false);
    }
  }, [root, openFile]);

  useEffect(() => {
    setFile(null);
    setView(null);
    setDiff(null);
    setHits(null);
    setOpen(new Set());
    void loadAll();
  }, [loadAll]);

  // Load the open file and, when it changed, its diff.
  const changeOf = useMemo(() => new Map(changes.map((c) => [c.path, c])), [changes]);
  const loadFile = useCallback(
    async (path: string) => {
      if (!root) return;
      setFileErr(null);
      const change = changeOf.get(path);
      const [v, d] = await Promise.all([
        change?.kind === "deleted" ? Promise.resolve(null) : explorer.read(root, path).catch((e) => {
          setFileErr(errorText(e));
          return null;
        }),
        change ? explorer.diff(root, path).catch(() => null) : Promise.resolve(null),
      ]);
      setView(v);
      setDiff(d);
      if (!v && d) setMode("diff");
    },
    [root, changeOf],
  );

  useEffect(() => {
    if (!file) return;
    setView(null);
    setDiff(null);
    void loadFile(file);
    // Only a new file or checkout reloads here; live changes reload through the poll below.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [file, root]);

  // Agents keep working while this is open: watch the working changes and follow them.
  useEffect(() => {
    if (!active || !root) return;
    const t = window.setInterval(async () => {
      try {
        const st = await explorer.status(root);
        const sig = st.map((c) => c.code + c.path).join("|");
        if (sig === statusSig.current) return;
        statusSig.current = sig;
        setChanges(st);
        setList(await explorer.files(root));
        if (file) void loadFile(file);
      } catch {
        // A checkout removed under us shows its error on the next manual refresh.
      }
    }, POLL_MS);
    return () => window.clearInterval(t);
  }, [active, root, file, loadFile]);

  // Ctrl+P: quick open. Ctrl+Shift+F: search the repository.
  useEffect(() => {
    if (!active) return;
    const onKey = (e: KeyboardEvent) => {
      if (!(e.ctrlKey || e.metaKey)) return;
      if (e.key.toLowerCase() === "p" && !e.shiftKey) {
        e.preventDefault();
        setSide("files");
        window.setTimeout(() => filterRef.current?.focus(), 0);
      } else if (e.key.toLowerCase() === "f" && e.shiftKey) {
        e.preventDefault();
        setSide("search");
        window.setTimeout(() => searchRef.current?.focus(), 0);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [active]);

  const runSearch = useCallback(async () => {
    if (!root || query.trim().length < 2) return;
    setSearching(true);
    try {
      setHits(await explorer.search(root, query, caseSensitive));
    } catch (e) {
      toast("error", errorText(e));
    } finally {
      setSearching(false);
    }
  }, [root, query, caseSensitive, toast]);

  const tree = useMemo(() => buildTree(list?.files ?? []), [list]);
  const matches = useMemo(() => quickOpen(list?.files ?? [], filter), [list, filter]);
  const lines = useMemo(() => (view?.text ?? "").replace(/\n$/, "").split("\n"), [view]);
  const lang = file ? languageFor(file) : null;
  const checkout = checkouts.find((c) => norm(c.path) === norm(root));
  const branch = list?.branch || checkout?.branch || "";

  const askChat = () => {
    if (!file) return;
    if (sel && view && !view.binary) {
      const a = Math.min(sel.from, sel.to);
      const b = Math.max(sel.from, sel.to);
      const code = excerpt(lines, a, b);
      onAskHermes(`In \`${file}\` ${a === b ? `line ${a}` : `lines ${a}-${b}`}:\n\n\`\`\`${lang ?? ""}\n${code}\n\`\`\`\n\n`, root);
    } else {
      onAskHermes(`Look at \`${file}\` and `, root);
    }
  };

  const openInEditor = async (path?: string, line?: number) => {
    try {
      await explorer.openEditor(root, path, line);
    } catch (e) {
      toast("error", errorText(e));
    }
  };

  if (!options.length) {
    return (
      <div className="grid h-full place-items-center p-8">
        <Empty icon={<FolderGit2 className="size-6" />} title="No repositories yet" hint="Explorer shows the Git repositories the fleet scan finds under your scan roots (Work: Fleet)." />
      </div>
    );
  }

  return (
    <div className="flex h-full min-h-0 flex-col">
      {/* Header: which repository and which checkout of it */}
      <div className="flex flex-wrap items-center gap-2 border-b border-line px-4 py-2.5">
        <FolderGit2 className="size-4 text-slate-500" />
        <select
          aria-label="Repository"
          value={repo}
          onChange={(e) => {
            setRepo(e.target.value);
            setRoot("");
          }}
          className="h-8 max-w-64 rounded-md border border-line bg-canvas px-2 text-xs text-slate-200"
        >
          {options.map((o) => (
            <option key={o.path} value={o.path}>
              {o.name}
            </option>
          ))}
        </select>
        {checkouts.length > 1 && (
          <select
            aria-label="Checkout"
            value={root}
            onChange={(e) => setRoot(e.target.value)}
            className="h-8 max-w-96 rounded-md border border-line bg-canvas px-2 text-xs text-slate-200"
            title="The repository itself, or the worktree a mission works in"
          >
            {checkouts.map((c) => (
              <option key={c.path} value={c.path}>
                {checkoutLabel(c.branch, c.main)}
              </option>
            ))}
          </select>
        )}
        {branch && (
          <Badge className="bg-white/5 text-slate-300 ring-white/10">
            <GitBranch className="mr-1 size-3" />
            {branch}
          </Badge>
        )}
        {checkouts.length > 1 && <span className="text-label text-slate-500">{checkouts.length - 1} agent worktree{checkouts.length === 2 ? "" : "s"}</span>}
        <span className="truncate text-label text-slate-600" title={root}>
          {root}
        </span>
        <div className="ml-auto flex items-center gap-1.5">
          <Button size="sm" variant="ghost" icon={<RefreshCw className={`size-3.5 ${loading ? "animate-spin" : ""}`} />} onClick={() => void loadAll()} disabled={!root}>
            Refresh
          </Button>
          <Button size="sm" variant="ghost" icon={<FolderOpen className="size-3.5" />} onClick={() => root && void studio.os.reveal(root).catch((e) => toast("error", errorText(e)))} disabled={!root}>
            Folder
          </Button>
          <Button size="sm" icon={<Code2 className="size-3.5" />} onClick={() => void openInEditor()} disabled={!root}>
            Open in VS Code
          </Button>
        </div>
      </div>

      <div className="flex min-h-0 flex-1">
        {/* Left: files, changes, search */}
        <aside className="flex w-72 shrink-0 flex-col border-r border-line bg-panel/40">
          <div className="flex border-b border-line text-xs">
            {(
              [
                ["files", "Files", list ? list.files.length : null],
                ["changes", "Changes", changes.length],
                ["search", "Search", hits ? hits.length : null],
              ] as [Side, string, number | null][]
            ).map(([id, label, n]) => (
              <button
                key={id}
                onClick={() => setSide(id)}
                className={`flex flex-1 items-center justify-center gap-1.5 py-2 ${side === id ? "border-b-2 border-emerald-500 text-slate-100" : "text-slate-500 hover:text-slate-300"}`}
              >
                {label}
                {n !== null && n > 0 && <span className={`rounded px-1 text-caption ${id === "changes" ? "bg-amber-500/15 text-amber-300" : "bg-white/5 text-slate-400"}`}>{n}</span>}
              </button>
            ))}
          </div>

          {side === "files" && (
            <>
              <div className="p-2">
                <input ref={filterRef} value={filter} onChange={(e) => setFilter(e.target.value)} placeholder="Go to file (Ctrl+P)" spellCheck={false} className={`${inputClass} h-8 py-1 text-xs`} />
              </div>
              <div className="min-h-0 flex-1 overflow-auto pb-3 text-xs">
                {loadErr && <div className="px-3 py-2 text-danger">{loadErr}</div>}
                {!list && !loadErr && <Loader2 className="mx-auto mt-8 size-4 animate-spin text-slate-500" />}
                {filter.trim()
                  ? matches.map((p) => (
                      <button key={p} onClick={() => openFile(p)} className={`flex w-full items-center gap-1.5 px-3 py-1 text-left hover:bg-white/5 ${file === p ? "bg-emerald-500/10 text-slate-100" : "text-slate-300"}`} title={p}>
                        <FileIcon path={p} />
                        <span className="truncate">{p.split("/").pop()}</span>
                        <span className="ml-auto truncate pl-2 text-caption text-slate-600">{p.split("/").slice(0, -1).join("/")}</span>
                      </button>
                    ))
                  : tree.map((n) => <TreeRow key={n.path} node={n} depth={0} open={open} setOpen={setOpen} file={file} onOpen={openFile} changes={changeOf} />)}
                {list?.truncated && <div className="px-3 py-2 text-caption text-slate-500">Showing the first {list.files.length.toLocaleString()} files. Use Go to file or Search for the rest.</div>}
                {list && !list.git && <div className="px-3 py-2 text-caption text-slate-500">Listed from the folder: Git did not answer, so ignore rules are not applied.</div>}
              </div>
            </>
          )}

          {side === "changes" && (
            <div className="min-h-0 flex-1 overflow-auto py-1 text-xs">
              {changes.length === 0 ? (
                <div className="p-3">
                  <Empty icon={<FileDiff className="size-5" />} title="No working changes" hint={checkout && !checkout.main ? "This worktree matches its last commit." : "The checkout matches its last commit. Pick a mission worktree above to see an agent's changes."} />
                </div>
              ) : (
                changes.map((c) => (
                  <button key={c.code + c.path} onClick={() => openFile(c.path, { mode: "diff" })} className={`flex w-full items-center gap-2 px-3 py-1 text-left hover:bg-white/5 ${file === c.path ? "bg-emerald-500/10" : ""}`} title={c.from ? `${c.from} -> ${c.path}` : c.path}>
                    <span className={`w-3 shrink-0 font-mono font-semibold ${KIND_STYLE[c.kind].cls}`}>{KIND_STYLE[c.kind].letter}</span>
                    <span className={`truncate ${c.kind === "deleted" ? "text-slate-500 line-through" : "text-slate-200"}`}>{c.path.split("/").pop()}</span>
                    <span className="ml-auto truncate pl-2 text-caption text-slate-600">{c.path.split("/").slice(0, -1).join("/")}</span>
                  </button>
                ))
              )}
            </div>
          )}

          {side === "search" && (
            <>
              <form
                className="space-y-1.5 p-2"
                onSubmit={(e) => {
                  e.preventDefault();
                  void runSearch();
                }}
              >
                <div className="flex gap-1.5">
                  <input ref={searchRef} value={query} onChange={(e) => setQuery(e.target.value)} placeholder="Search the repository" spellCheck={false} className={`${inputClass} h-8 py-1 text-xs`} />
                  <Button size="sm" type="submit" busy={searching} icon={<Search className="size-3.5" />} disabled={query.trim().length < 2} aria-label="Search" />
                </div>
                <label className="flex items-center gap-1.5 text-caption text-slate-500">
                  <input type="checkbox" checked={caseSensitive} onChange={(e) => setCaseSensitive(e.target.checked)} />
                  Match case
                </label>
              </form>
              <div className="min-h-0 flex-1 overflow-auto pb-3 text-xs">
                {hits && hits.length === 0 && <div className="px-3 py-2 text-slate-500">Nothing found.</div>}
                {hits && <SearchResults hits={hits} file={file} onOpen={(p, line) => openFile(p, { line })} />}
                {hits && hits.length >= 500 && <div className="px-3 py-2 text-caption text-slate-500">First 500 matches shown. Narrow the search for more.</div>}
              </div>
            </>
          )}
        </aside>

        {/* Right: the open file */}
        <section className="flex min-w-0 flex-1 flex-col">
          {!file ? (
            <div className="grid flex-1 place-items-center p-8">
              <div className="max-w-md text-center">
                <Code2 className="mx-auto mb-3 size-8 text-slate-600" />
                <div className="text-sm font-medium text-slate-300">Pick a file to read it</div>
                <p className="mt-1.5 text-label leading-relaxed text-slate-500">
                  Explorer is read-only on purpose: see what the agents see and what they changed, then hand a file or a few lines to Chat, or open it in VS Code to edit. Ctrl+P finds a file, Ctrl+Shift+F searches.
                </p>
              </div>
            </div>
          ) : (
            <>
              <FileBar
                file={file}
                view={view}
                change={changeOf.get(file)}
                diffText={diff}
                mode={mode}
                setMode={setMode}
                sel={sel}
                onCopy={() => void navigator.clipboard.writeText(file).then(() => toast("ok", "Path copied"))}
                onAsk={askChat}
                onEditor={() => void openInEditor(file, sel ? Math.min(sel.from, sel.to) : undefined)}
              />
              <div className="min-h-0 flex-1 overflow-auto bg-inset">
                {fileErr && mode !== "diff" && <div className="p-4 text-xs text-danger">{fileErr}</div>}
                {mode === "diff" && diff !== null && <DiffView text={diff} />}
                {mode === "diff" && diff === null && <Loader2 className="mx-auto mt-10 size-5 animate-spin text-slate-500" />}
                {mode === "preview" && view && <div className="prose-studio max-w-4xl p-6"><Markdown text={view.text} workingDir={root} /></div>}
                {mode === "code" && !view && !fileErr && <Loader2 className="mx-auto mt-10 size-5 animate-spin text-slate-500" />}
                {mode === "code" && view?.binary && <div className="p-6 text-xs text-slate-500">Binary file ({fileSize(view.size)}). Open it in VS Code or the folder to view it.</div>}
                {mode === "code" && view && !view.binary && <CodeView key={file + root} lines={lines} text={view.text} lang={lang} sel={sel} setSel={setSel} jump={jump} />}
                {mode === "code" && view?.truncated && <div className="border-t border-line p-3 text-caption text-slate-500">Showing the first 512 KB of {fileSize(view.size)}. Open in VS Code for the rest.</div>}
              </div>
            </>
          )}
        </section>
      </div>
    </div>
  );
}

function TreeRow({
  node,
  depth,
  open,
  setOpen,
  file,
  onOpen,
  changes,
}: {
  node: TreeNode;
  depth: number;
  open: Set<string>;
  setOpen: (f: (s: Set<string>) => Set<string>) => void;
  file: string | null;
  onOpen: (p: string) => void;
  changes: Map<string, ExplorerChange>;
}) {
  const pad = { paddingLeft: `${8 + depth * 12}px` };
  if (node.dir) {
    const isOpen = open.has(node.path);
    const changed = !isOpen && [...changes.keys()].some((p) => p.startsWith(node.path + "/"));
    return (
      <>
        <button
          style={pad}
          onClick={() =>
            setOpen((s) => {
              const next = new Set(s);
              if (next.has(node.path)) next.delete(node.path);
              else next.add(node.path);
              return next;
            })
          }
          className="flex w-full items-center gap-1 py-[3px] pr-2 text-left text-slate-300 hover:bg-white/5"
        >
          {isOpen ? <ChevronDown className="size-3 shrink-0 text-slate-500" /> : <ChevronRight className="size-3 shrink-0 text-slate-500" />}
          {isOpen ? <FolderOpen className="size-3.5 shrink-0 text-sky-400/70" /> : <Folder className="size-3.5 shrink-0 text-sky-400/70" />}
          <span className="truncate">{node.name}</span>
          {changed && <span className="ml-auto size-1.5 shrink-0 rounded-full bg-amber-400" title="Contains changes" />}
        </button>
        {isOpen && node.children.map((c) => <TreeRow key={c.path} node={c} depth={depth + 1} open={open} setOpen={setOpen} file={file} onOpen={onOpen} changes={changes} />)}
      </>
    );
  }
  const ch = changes.get(node.path);
  return (
    <button style={{ paddingLeft: `${20 + depth * 12}px` }} onClick={() => onOpen(node.path)} className={`flex w-full items-center gap-1.5 py-[3px] pr-2 text-left hover:bg-white/5 ${file === node.path ? "bg-emerald-500/10 text-slate-100" : "text-slate-400"}`} title={node.path}>
      <FileIcon path={node.path} />
      <span className={`truncate ${ch ? KIND_STYLE[ch.kind].cls : ""}`}>{node.name}</span>
      {ch && <span className={`ml-auto shrink-0 font-mono text-caption ${KIND_STYLE[ch.kind].cls}`}>{KIND_STYLE[ch.kind].letter}</span>}
    </button>
  );
}

function SearchResults({ hits, file, onOpen }: { hits: ExplorerHit[]; file: string | null; onOpen: (p: string, line: number) => void }) {
  const groups = useMemo(() => {
    const m = new Map<string, ExplorerHit[]>();
    for (const h of hits) m.set(h.path, [...(m.get(h.path) ?? []), h]);
    return [...m.entries()];
  }, [hits]);
  return (
    <>
      {groups.map(([path, hs]) => (
        <div key={path} className="mb-1">
          <div className={`flex items-center gap-1.5 px-3 py-1 ${file === path ? "text-slate-100" : "text-slate-300"}`} title={path}>
            <FileIcon path={path} />
            <span className="truncate font-medium">{path.split("/").pop()}</span>
            <span className="truncate text-caption text-slate-600">{path.split("/").slice(0, -1).join("/")}</span>
            <span className="ml-auto text-caption text-slate-500">{hs.length}</span>
          </div>
          {hs.map((h) => (
            <button key={h.line} onClick={() => onOpen(path, h.line)} className="flex w-full gap-2 py-0.5 pl-8 pr-2 text-left font-mono text-caption text-slate-400 hover:bg-white/5">
              <span className="w-8 shrink-0 text-right text-slate-600">{h.line}</span>
              <span className="truncate">{h.text}</span>
            </button>
          ))}
        </div>
      ))}
    </>
  );
}

function FileBar({
  file,
  view,
  change,
  diffText,
  mode,
  setMode,
  sel,
  onCopy,
  onAsk,
  onEditor,
}: {
  file: string;
  view: ExplorerFile | null;
  change: ExplorerChange | undefined;
  diffText: string | null;
  mode: Mode;
  setMode: (m: Mode) => void;
  sel: Sel;
  onCopy: () => void;
  onAsk: () => void;
  onEditor: () => void;
}) {
  const stat = useMemo(() => (diffText ? diffStat(parseDiff(diffText)) : null), [diffText]);
  const markdown = languageFor(file) === "markdown";
  const tab = (m: Mode, label: string, icon: ReactNode, disabled = false) => (
    <button
      key={m}
      disabled={disabled}
      onClick={() => setMode(m)}
      className={`inline-flex items-center gap-1 rounded-md px-2 py-1 text-xs disabled:opacity-40 ${mode === m ? "bg-white/10 text-slate-100" : "text-slate-500 hover:text-slate-300"}`}
    >
      {icon}
      {label}
    </button>
  );
  const lines = sel ? (sel.from === sel.to ? `line ${sel.from}` : `lines ${Math.min(sel.from, sel.to)}-${Math.max(sel.from, sel.to)}`) : null;
  return (
    <div className="flex flex-wrap items-center gap-2 border-b border-line px-4 py-2">
      <div className="flex min-w-0 items-center gap-1 text-xs">
        {file.split("/").map((part, i, all) => (
          <span key={i} className={`truncate ${i === all.length - 1 ? "font-medium text-slate-100" : "text-slate-500"}`}>
            {part}
            {i < all.length - 1 && <span className="px-0.5 text-slate-700">/</span>}
          </span>
        ))}
      </div>
      {change && <Badge className={`bg-white/5 ring-white/10 ${KIND_STYLE[change.kind].cls}`}>{change.kind}</Badge>}
      {stat && (
        <span className="font-mono text-caption">
          <span className="text-emerald-400">+{stat.add}</span> <span className="text-red-400">-{stat.del}</span>
        </span>
      )}
      {view && !view.binary && <span className="text-caption text-slate-600">{fileSize(view.size)}</span>}
      <div className="ml-auto flex items-center gap-1">
        <div className="mr-1 flex items-center gap-0.5 rounded-lg border border-line p-0.5">
          {tab("code", "File", <FileCode2 className="size-3.5" />, change?.kind === "deleted")}
          {change && tab("diff", "Changes", <FileDiff className="size-3.5" />)}
          {markdown && tab("preview", "Preview", <Eye className="size-3.5" />, !view)}
        </div>
        <Button size="sm" variant="ghost" icon={<Copy className="size-3.5" />} onClick={onCopy} title="Copy the path">
          Path
        </Button>
        <Button size="sm" variant="ghost" icon={<MessageSquarePlus className="size-3.5" />} onClick={onAsk} title={lines ? `Put ${lines} into a Chat prompt` : "Start a Chat prompt about this file. Click line numbers to pick lines first."}>
          {lines ? `Ask about ${lines}` : "Ask in Chat"}
        </Button>
        <Button size="sm" variant="ghost" icon={<SquareArrowOutUpRight className="size-3.5" />} onClick={onEditor} title="Open in VS Code at this line">
          VS Code
        </Button>
      </div>
    </div>
  );
}

const CodeView = memo(function CodeView({
  lines,
  text,
  lang,
  sel,
  setSel,
  jump,
}: {
  lines: string[];
  text: string;
  lang: string | null;
  sel: Sel;
  setSel: (s: Sel) => void;
  jump: { line: number; nonce: number } | null;
}) {
  const html = useMemo(() => {
    if (lang && lines.length <= HIGHLIGHT_MAX_LINES && hljs.getLanguage(lang)) {
      try {
        return splitHighlighted(hljs.highlight(text.replace(/\n$/, ""), { language: lang, ignoreIllegals: true }).value);
      } catch {
        // Fall through to plain text.
      }
    }
    return lines.map(escapeHtml);
  }, [lines, text, lang]);

  const anchor = useRef<number | null>(null);
  useEffect(() => {
    if (!jump) return;
    const t = window.setTimeout(() => document.getElementById(`ln-${jump.line}`)?.scrollIntoView({ block: "center" }), 30);
    return () => window.clearTimeout(t);
  }, [jump]);

  const lo = sel ? Math.min(sel.from, sel.to) : 0;
  const hi = sel ? Math.max(sel.from, sel.to) : -1;
  const width = String(lines.length).length;
  return (
    <div className="min-w-max py-2 font-mono text-[12.5px] leading-5">
      {html.map((h, i) => {
        const n = i + 1;
        const picked = n >= lo && n <= hi;
        return (
          <div key={i} id={`ln-${n}`} className={`flex ${picked ? "bg-emerald-500/10" : "hover:bg-white/[0.03]"}`}>
            <button
              onClick={(e) => {
                if (e.shiftKey && anchor.current !== null) {
                  setSel({ from: anchor.current, to: n });
                } else if (sel && sel.from === n && sel.to === n) {
                  anchor.current = null;
                  setSel(null);
                } else {
                  anchor.current = n;
                  setSel({ from: n, to: n });
                }
              }}
              className={`sticky left-0 shrink-0 select-none border-r border-line bg-inset pl-3 pr-3 text-right ${picked ? "text-emerald-300" : "text-slate-600 hover:text-slate-400"}`}
              style={{ width: `${width + 3}ch` }}
              title="Click to pick this line, Shift+click to pick a range"
            >
              {n}
            </button>
            <code className="hljs whitespace-pre pl-4 pr-6" dangerouslySetInnerHTML={{ __html: h || " " }} />
          </div>
        );
      })}
    </div>
  );
});

const DiffView = memo(function DiffView({ text }: { text: string }) {
  const rows = useMemo(() => parseDiff(text), [text]);
  if (!text.trim()) return <div className="p-6 text-xs text-slate-500">No textual changes.</div>;
  return (
    <div className="min-w-max py-2 font-mono text-[12.5px] leading-5">
      {rows.map((r, i) => {
        const cls =
          r.kind === "add"
            ? "bg-emerald-500/10 text-emerald-100"
            : r.kind === "del"
              ? "bg-red-500/10 text-red-100"
              : r.kind === "hunk"
                ? "bg-sky-500/5 text-sky-300"
                : r.kind === "meta"
                  ? "text-slate-500"
                  : "text-slate-300";
        const sign = r.kind === "add" ? "+" : r.kind === "del" ? "-" : " ";
        return (
          <div key={i} className={`flex ${cls}`}>
            <span className="w-12 shrink-0 select-none pr-2 text-right text-slate-600">{r.old ?? ""}</span>
            <span className="w-12 shrink-0 select-none border-r border-line pr-2 text-right text-slate-600">{r.new ?? ""}</span>
            <span className="w-5 shrink-0 select-none text-center opacity-70">{r.kind === "hunk" || r.kind === "meta" ? "" : sign}</span>
            <span className="whitespace-pre pr-6">{r.text || " "}</span>
          </div>
        );
      })}
    </div>
  );
});
