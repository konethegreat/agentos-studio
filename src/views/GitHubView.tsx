import {
  Bot,
  CheckCircle2,
  CircleDot,
  CircleSlash,
  GitBranch,
  GitCommitHorizontal,
  GitMerge,
  GitPullRequest,
  GitPullRequestCreate,
  Loader2,
  LogIn,
  MessageSquare,
  Play,
  RefreshCw,
  Search,
  XCircle,
} from "lucide-react";
import { useCallback, useEffect, useMemo, useState, type ReactNode } from "react";
import { ApprovalDeclined, useGuarded } from "../components/ApprovalProvider";
import { goTo } from "../lib/nav";
import { Markdown } from "../components/Markdown";
import { useToast } from "../components/Toasts";
import { Badge, Button, Card, ExternalLink, Field, Modal, inputClass } from "../components/ui";
import { errorText, type Provenance, type RepoInfo, api } from "../lib/api";
import { ProvenanceCard } from "../components/Provenance";
import { relativeTime } from "../lib/chat";
import {
  studio,
  type GhAuth,
  type GhBranch,
  type GhCheck,
  type GhCommit,
  type GhIssue,
  type GhIssueDetail,
  type GhPr,
  type GhPrDetail,
  type GhRepo,
  type GhRun,
  type GhRunDetail,
} from "../lib/studio";

type Tab = "prs" | "issues" | "actions" | "branches" | "commits";

const ts = (iso: string) => relativeTime(new Date(iso).getTime() / 1000);

function checkState(c: GhCheck): "ok" | "fail" | "pending" | "neutral" {
  const v = (c.conclusion || c.state || c.status || "").toUpperCase();
  if (["SUCCESS", "COMPLETED_SUCCESS"].includes(v)) return "ok";
  if (["FAILURE", "ERROR", "TIMED_OUT", "CANCELLED", "ACTION_REQUIRED", "STARTUP_FAILURE"].includes(v)) return "fail";
  if (["SKIPPED", "NEUTRAL", "STALE"].includes(v)) return "neutral";
  return "pending";
}

function ChecksSummary({ checks }: { checks: GhCheck[] | null }) {
  if (!checks?.length) return null;
  const s = checks.map(checkState);
  const fail = s.filter((x) => x === "fail").length;
  const pend = s.filter((x) => x === "pending").length;
  if (fail) return <Badge className="bg-red-500/10 text-red-300 ring-red-500/30">{fail} failing</Badge>;
  if (pend) return <Badge className="bg-amber-500/10 text-amber-300 ring-amber-500/30">{pend} running</Badge>;
  return <Badge className="bg-emerald-500/10 text-emerald-300 ring-emerald-500/30">checks pass</Badge>;
}

function RunIcon({ status, conclusion }: { status: string; conclusion: string }) {
  if (status !== "completed") return <Loader2 className="size-3.5 animate-spin text-amber-300" />;
  if (conclusion === "success") return <CheckCircle2 className="size-3.5 text-ok" />;
  if (conclusion === "skipped" || conclusion === "cancelled") return <CircleSlash className="size-3.5 text-slate-500" />;
  return <XCircle className="size-3.5 text-danger" />;
}

export function GitHubView({ repos, onAskHermes }: { repos: RepoInfo[]; onAskHermes: (prompt: string, cwd?: string) => void }) {
  const toast = useToast();
  const guarded = useGuarded();
  const [auth, setAuth] = useState<GhAuth | null>(null);
  const [all, setAll] = useState<GhRepo[]>([]);
  const [filter, setFilter] = useState("");
  const [repo, setRepo] = useState<string>(() => localStorage.getItem("gh-repo") ?? "");
  const [tab, setTab] = useState<Tab>("prs");
  const [state, setState] = useState("open");
  const [loading, setLoading] = useState(false);
  const [prs, setPrs] = useState<GhPr[]>([]);
  const [issues, setIssues] = useState<GhIssue[]>([]);
  const [runs, setRuns] = useState<GhRun[]>([]);
  const [branches, setBranches] = useState<GhBranch[]>([]);
  const [commits, setCommits] = useState<GhCommit[]>([]);
  const [branch, setBranch] = useState<string>("");
  const [sel, setSel] = useState<{ kind: Tab; id: number } | null>(null);
  const [creating, setCreating] = useState<"issue" | "pr" | null>(null);

  // Local clones, so PRs can be opened from a folder and Hermes can work inside it.
  const localByRepo = useMemo(() => {
    const m = new Map<string, string>();
    for (const r of repos) {
      const match = r.remote.match(/github\.com[:/]([^/]+\/[^/.]+?)(\.git)?$/i);
      if (match) m.set(match[1].toLowerCase(), r.path);
    }
    return m;
  }, [repos]);
  const localPath = localByRepo.get(repo.toLowerCase());

  const loadAuth = useCallback(async () => {
    try {
      const a = await studio.gh.auth();
      setAuth(a);
      if (a.logged_in) setAll(await studio.gh.repos(200));
    } catch (e) {
      toast("error", errorText(e));
    }
  }, [toast]);

  useEffect(() => { void loadAuth(); }, [loadAuth]);
  useEffect(() => {
    if (!repo && all.length) setRepo(all[0].nameWithOwner);
  }, [all, repo]);
  useEffect(() => {
    if (repo) localStorage.setItem("gh-repo", repo);
  }, [repo]);

  const load = useCallback(async () => {
    if (!repo || !auth?.logged_in) return;
    setLoading(true);
    try {
      if (tab === "prs") setPrs(await studio.gh.prs(repo, state));
      if (tab === "issues") setIssues(await studio.gh.issues(repo, state === "merged" ? "closed" : state));
      if (tab === "actions") setRuns(await studio.gh.runs(repo, branch || null));
      if (tab === "branches") setBranches(await studio.gh.branches(repo));
      if (tab === "commits") {
        if (!branches.length) setBranches(await studio.gh.branches(repo));
        setCommits(await studio.gh.commits(repo, branch || null));
      }
    } catch (e) {
      toast("error", errorText(e));
    } finally {
      setLoading(false);
    }
  }, [auth?.logged_in, branch, branches.length, repo, state, tab, toast]);

  useEffect(() => { void load(); }, [load]);
  useEffect(() => { setSel(null); }, [repo, tab]);

  async function act(p: (a: Approved) => Promise<string>) {
    try {
      const msg = await guarded((a) => p(a));
      toast("ok", msg || "Done");
      void load();
      return true;
    } catch (e) {
      if (!(e instanceof ApprovalDeclined)) toast("error", errorText(e));
      return false;
    }
  }

  const shown = all.filter((r) => !filter || r.nameWithOwner.toLowerCase().includes(filter.toLowerCase()));

  if (auth && !auth.installed)
    return (
      <Centered title="GitHub CLI not installed" hint="Studio reads GitHub through the GitHub CLI (gh). Setup installs it and signs you in.">
        <div className="mt-4 flex justify-center gap-2">
          <Button variant="primary" onClick={() => goTo("setup")}>
            Open Setup
          </Button>
          <Button icon={<RefreshCw className="size-4" />} onClick={() => void loadAuth()}>
            Check again
          </Button>
        </div>
      </Centered>
    );
  if (auth && !auth.logged_in)
    return (
      <Centered title="Sign in to GitHub" hint="Studio uses your gh login. Your token stays in gh's own keyring.">
        <div className="mt-4 flex justify-center gap-2">
          <Button variant="primary" icon={<LogIn className="size-4" />} onClick={() => void studio.gh.login().then((m) => toast("info", m)).catch((e) => toast("error", errorText(e)))}>
            Sign in with gh
          </Button>
          <Button icon={<RefreshCw className="size-4" />} onClick={() => void loadAuth()}>
            Refresh
          </Button>
        </div>
      </Centered>
    );

  return (
    <div className="flex h-full min-h-0">
      <aside className="flex w-64 shrink-0 flex-col border-r border-line bg-inset/60">
        <div className="space-y-2 p-3">
          <div className="flex items-center justify-between text-label text-slate-500">
            <span title={auth?.scopes ? `Scopes: ${auth.scopes}` : undefined}>{auth ? `Signed in as ${auth.login}` : "Checking gh..."}</span>
            <button onClick={() => void loadAuth()} className="rounded p-1 hover:bg-white/5 hover:text-slate-200" title="Reload repositories" aria-label="Reload repositories">
              <RefreshCw className="size-3" />
            </button>
          </div>
          <div className="flex items-center gap-2 rounded-lg bg-card px-2 ring-1 ring-line">
            <Search className="size-3.5 text-slate-500" />
            <input value={filter} onChange={(e) => setFilter(e.target.value)} placeholder="Filter repositories" aria-label="Filter repositories" className="h-8 min-w-0 flex-1 bg-transparent text-xs text-slate-100 outline-none placeholder:text-slate-600" />
          </div>
        </div>
        <div className="min-h-0 flex-1 overflow-y-auto px-2 pb-3">
          {shown.map((r) => (
            <button
              key={r.nameWithOwner}
              onClick={() => setRepo(r.nameWithOwner)}
              className={`mb-0.5 block w-full rounded-lg px-2.5 py-1.5 text-left ${repo === r.nameWithOwner ? "bg-white/[0.07]" : "hover:bg-white/[0.03]"}`}
              title={r.description ?? r.nameWithOwner}
            >
              <div className="truncate text-ui text-slate-200">{r.nameWithOwner}</div>
              <div className="flex gap-1.5 text-2xs text-slate-500">
                {r.isPrivate && <span>private</span>}
                {r.primaryLanguage && <span>{r.primaryLanguage.name}</span>}
                <span>{ts(r.updatedAt)}</span>
                {localByRepo.has(r.nameWithOwner.toLowerCase()) && <span className="text-ok/80">local</span>}
              </div>
            </button>
          ))}
        </div>
      </aside>

      <section className="flex min-w-0 flex-1 flex-col">
        <header className="flex h-14 shrink-0 items-center gap-3 border-b border-line px-5">
          <div className="min-w-0 flex-1">
            <div className="truncate text-sm font-semibold text-slate-100">{repo || "Pick a repository"}</div>
            <div className="truncate text-label text-slate-500">{localPath ? `Local clone: ${localPath}` : "No local clone found by the fleet scan"}</div>
          </div>
          <Button size="sm" icon={<CircleDot className="size-3.5" />} disabled={!repo} onClick={() => setCreating("issue")} title="Open a new issue">
            New issue
          </Button>
          <Button size="sm" icon={<GitPullRequestCreate className="size-3.5" />} disabled={!localPath} onClick={() => setCreating("pr")} title={localPath ? "Open a pull request from the current branch of the local clone" : "Needs a local clone"}>
            New PR
          </Button>
          <Button size="sm" variant="ghost" icon={loading ? <Loader2 className="size-3.5 animate-spin" /> : <RefreshCw className="size-3.5" />} onClick={() => void load()} title="Reload">
            Reload
          </Button>
        </header>
        <div className="flex shrink-0 items-center gap-1 border-b border-line px-5 py-2" role="tablist">
          {(
            [
              ["prs", "Pull requests", <GitPullRequest key="i" className="size-3.5" />],
              ["issues", "Issues", <CircleDot key="i" className="size-3.5" />],
              ["actions", "Actions", <Play key="i" className="size-3.5" />],
              ["branches", "Branches", <GitBranch key="i" className="size-3.5" />],
              ["commits", "Commits", <GitCommitHorizontal key="i" className="size-3.5" />],
            ] as [Tab, string, ReactNode][]
          ).map(([id, label, icon]) => (
            <button key={id} role="tab" aria-selected={tab === id} onClick={() => setTab(id)} className={`inline-flex h-7 items-center gap-1.5 rounded-md px-2.5 text-xs ${tab === id ? "bg-white/[0.07] text-slate-100" : "text-slate-400 hover:text-slate-200"}`}>
              {icon}
              {label}
            </button>
          ))}
          <div className="flex-1" />
          {(tab === "prs" || tab === "issues") && (
            <select value={state} onChange={(e) => setState(e.target.value)} className="h-7 rounded-md border border-line bg-card px-2 text-label text-slate-300" aria-label="State filter">
              <option value="open">Open</option>
              <option value="closed">Closed</option>
              {tab === "prs" && <option value="merged">Merged</option>}
              <option value="all">All</option>
            </select>
          )}
          {(tab === "actions" || tab === "commits") && (
            <input value={branch} onChange={(e) => setBranch(e.target.value)} onKeyDown={(e) => e.key === "Enter" && void load()} placeholder="branch (default)" aria-label="Branch" list="gh-branches" className="h-7 w-44 rounded-md border border-line bg-card px-2 text-label text-slate-300 outline-none" />
          )}
          <datalist id="gh-branches">
            {branches.map((b) => (
              <option key={b.name} value={b.name} />
            ))}
          </datalist>
        </div>

        <div className="flex min-h-0 flex-1">
          <div className={`min-h-0 overflow-y-auto ${sel ? "w-[42%] border-r border-line" : "flex-1"}`}>
            {loading && <Loader2 className="mx-auto mt-10 size-5 animate-spin text-slate-500" />}
            {!loading && tab === "prs" && (
              <List empty="No pull requests">
                {prs.map((p) => (
                  <Row key={p.number} active={sel?.id === p.number} onClick={() => setSel({ kind: "prs", id: p.number })}>
                    <div className="flex items-center gap-2">
                      {p.state === "MERGED" ? <GitMerge className="size-3.5 text-violet-300" /> : <GitPullRequest className={`size-3.5 ${p.state === "OPEN" ? "text-ok" : "text-danger"}`} />}
                      <span className="truncate text-body-sm text-slate-200">{p.title}</span>
                    </div>
                    <div className="mt-0.5 flex flex-wrap items-center gap-1.5 pl-5 text-caption text-slate-500">
                      <span>#{p.number}</span>
                      <span>{p.author?.login}</span>
                      <span className="font-mono">
                        {p.headRefName} {"->"} {p.baseRefName}
                      </span>
                      <span className="text-ok/80">+{p.additions}</span>
                      <span className="text-danger/80">-{p.deletions}</span>
                      {p.isDraft && <Badge className="bg-white/5 text-slate-400 ring-white/10">draft</Badge>}
                      <ChecksSummary checks={p.statusCheckRollup} />
                      <span>{ts(p.updatedAt)}</span>
                    </div>
                  </Row>
                ))}
              </List>
            )}
            {!loading && tab === "issues" && (
              <List empty="No issues">
                {issues.map((i) => (
                  <Row key={i.number} active={sel?.id === i.number} onClick={() => setSel({ kind: "issues", id: i.number })}>
                    <div className="flex items-center gap-2">
                      <CircleDot className={`size-3.5 ${i.state === "OPEN" ? "text-ok" : "text-violet-300"}`} />
                      <span className="truncate text-body-sm text-slate-200">{i.title}</span>
                    </div>
                    <div className="mt-0.5 flex flex-wrap gap-1.5 pl-5 text-caption text-slate-500">
                      <span>#{i.number}</span>
                      <span>{i.author?.login}</span>
                      {i.labels.map((l) => (
                        <span key={l.name} className="rounded bg-sky-500/10 px-1 text-sky-300">
                          {l.name}
                        </span>
                      ))}
                      <span>{ts(i.updatedAt)}</span>
                    </div>
                  </Row>
                ))}
              </List>
            )}
            {!loading && tab === "actions" && (
              <List empty="No workflow runs">
                {runs.map((r) => (
                  <Row key={r.databaseId} active={sel?.id === r.databaseId} onClick={() => setSel({ kind: "actions", id: r.databaseId })}>
                    <div className="flex items-center gap-2">
                      <RunIcon status={r.status} conclusion={r.conclusion} />
                      <span className="truncate text-body-sm text-slate-200">{r.displayTitle}</span>
                    </div>
                    <div className="mt-0.5 flex flex-wrap gap-1.5 pl-5 text-caption text-slate-500">
                      <span>{r.workflowName}</span>
                      <span className="font-mono">{r.headBranch}</span>
                      <span>{r.event}</span>
                      <span>{ts(r.createdAt)}</span>
                    </div>
                  </Row>
                ))}
              </List>
            )}
            {!loading && tab === "branches" && (
              <List empty="No branches">
                {branches.map((b) => (
                  <Row
                    key={b.name}
                    onClick={() => {
                      setBranch(b.name);
                      setTab("commits");
                    }}
                  >
                    <div className="flex items-center gap-2 text-body-sm text-slate-200">
                      <GitBranch className="size-3.5 text-slate-400" />
                      <span className="font-mono">{b.name}</span>
                      {b.protected && <Badge className="bg-amber-500/10 text-amber-300 ring-amber-500/30">protected</Badge>}
                      <span className="ml-auto font-mono text-caption text-slate-500">{b.commit.sha.slice(0, 7)}</span>
                    </div>
                  </Row>
                ))}
              </List>
            )}
            {!loading && tab === "commits" && (
              <List empty="No commits">
                {commits.map((c) => (
                  <Row key={c.sha}>
                    <div className="truncate text-body-sm text-slate-200">{c.commit.message.split("\n")[0]}</div>
                    <div className="mt-0.5 flex gap-1.5 text-caption text-slate-500">
                      <ExternalLink href={c.html_url} className="font-mono no-underline">
                        {c.sha.slice(0, 7)}
                      </ExternalLink>
                      <span>{c.author?.login ?? c.commit.author.name}</span>
                      <span>{ts(c.commit.author.date)}</span>
                    </div>
                  </Row>
                ))}
              </List>
            )}
          </div>
          {sel && (
            <div className="min-h-0 min-w-0 flex-1 overflow-y-auto">
              {sel.kind === "prs" && <PrDetail repo={repo} number={sel.id} act={act} localPath={localPath} onAskHermes={onAskHermes} />}
              {sel.kind === "issues" && <IssueDetail repo={repo} number={sel.id} act={act} localPath={localPath} onAskHermes={onAskHermes} />}
              {sel.kind === "actions" && <RunDetail repo={repo} id={sel.id} act={act} localPath={localPath} onAskHermes={onAskHermes} />}
            </div>
          )}
        </div>
      </section>
      {creating === "issue" && <CreateIssue repo={repo} onClose={() => setCreating(null)} act={act} />}
      {creating === "pr" && localPath && <CreatePr dir={localPath} onClose={() => setCreating(null)} act={act} />}
    </div>
  );
}

type Approved = { approved?: boolean };
type Act = (p: (a: Approved) => Promise<string>) => Promise<boolean>;
type DetailProps = { repo: string; act: Act; localPath?: string; onAskHermes: (prompt: string, cwd?: string) => void };

function Centered({ title, hint, children }: { title: string; hint: string; children?: ReactNode }) {
  return (
    <div className="grid h-full place-items-center">
      <Card className="max-w-md p-6 text-center">
        <h2 className="text-base font-semibold text-slate-100">{title}</h2>
        <p className="mt-1 text-xs text-slate-500">{hint}</p>
        {children}
      </Card>
    </div>
  );
}

function List({ children, empty }: { children: ReactNode[]; empty: string }) {
  if (!children.length) return <div className="px-5 py-10 text-center text-xs text-slate-600">{empty}</div>;
  return <div className="divide-y divide-line">{children}</div>;
}

function Row({ children, active, onClick }: { children: ReactNode; active?: boolean; onClick?: () => void }) {
  return (
    <button onClick={onClick} disabled={!onClick} className={`block w-full px-5 py-2.5 text-left ${active ? "bg-white/[0.06]" : onClick ? "hover:bg-white/[0.03]" : ""}`}>
      {children}
    </button>
  );
}

function CommentBox({ onSubmit, placeholder, label = "Comment" }: { onSubmit: (body: string) => Promise<boolean>; placeholder: string; label?: string }) {
  const [body, setBody] = useState("");
  const [busy, setBusy] = useState(false);
  return (
    <div className="space-y-2">
      <textarea value={body} onChange={(e) => setBody(e.target.value)} rows={3} placeholder={placeholder} className={`${inputClass} resize-y`} aria-label={label} />
      <div className="flex justify-end">
        <Button
          size="sm"
          icon={<MessageSquare className="size-3.5" />}
          busy={busy}
          disabled={!body.trim()}
          onClick={async () => {
            setBusy(true);
            if (await onSubmit(body)) setBody("");
            setBusy(false);
          }}
        >
          {label}
        </Button>
      </div>
    </div>
  );
}

function Thread({ items }: { items: { author: { login: string }; body: string; createdAt?: string; submittedAt?: string; state?: string }[] }) {
  return (
    <div className="space-y-2">
      {items
        .filter((c) => c.body?.trim() || c.state)
        .map((c, i) => (
          <div key={i} className="rounded-lg border border-line bg-canvas/60 px-3 py-2">
            <div className="mb-1 flex gap-2 text-caption text-slate-500">
              <span className="font-medium text-slate-300">{c.author?.login}</span>
              {c.state && <span>{c.state.toLowerCase().replace("_", " ")}</span>}
              <span>{ts(c.createdAt ?? c.submittedAt ?? new Date().toISOString())}</span>
            </div>
            {c.body?.trim() && <Markdown text={c.body} workingDir="" />}
          </div>
        ))}
    </div>
  );
}

interface DiffRow {
  text: string;
  /** The new-file path and line, when the row exists in the new file. */
  file?: string;
  line?: number;
}

/** Walk a unified diff and remember the new-file line each added or context row maps to. */
function parseDiff(diff: string): DiffRow[] {
  const rows: DiffRow[] = [];
  let file: string | undefined;
  let next = 0;
  for (const text of diff.split("\n")) {
    if (text.startsWith("diff --git ")) {
      file = undefined;
      rows.push({ text });
      continue;
    }
    if (text.startsWith("+++ /dev/null")) {
      file = undefined;
      rows.push({ text });
      continue;
    }
    if (text.startsWith("+++ b/")) {
      file = text.slice(6);
      rows.push({ text });
      continue;
    }
    if (text.startsWith("@@")) {
      const m = /^@@ -\d+(?:,\d+)? \+(\d+)(?:,\d+)? @@/.exec(text);
      next = m ? Number(m[1]) : 0;
      rows.push({ text });
      continue;
    }
    if (text.startsWith("+")) rows.push({ text, file, line: next++ });
    else if (text.startsWith(" ")) rows.push({ text, file, line: next++ });
    else rows.push({ text });
  }
  return rows;
}

function PrDetail({ repo, number, act, localPath, onAskHermes }: DetailProps & { number: number }) {
  const toast = useToast();
  const [pr, setPr] = useState<GhPrDetail | null>(null);
  const [diff, setDiff] = useState<string | null>(null);
  const [why, setWhy] = useState<Provenance | null>(null);
  const [busyWhy, setBusyWhy] = useState(false);
  const [method, setMethod] = useState("squash");
  const [del, setDel] = useState(true);
  const [review, setReview] = useState("comment");
  const reload = useCallback(() => {
    studio.gh
      .pr(repo, number)
      .then(setPr)
      .catch((e) => toast("error", errorText(e)));
  }, [number, repo, toast]);
  useEffect(() => {
    setPr(null);
    setDiff(null);
    setWhy(null);
    reload();
  }, [reload]);

  async function whyHere(file: string, line: number) {
    if (!localPath) return;
    setBusyWhy(true);
    try {
      setWhy(await api.fileProvenance(localPath, file, line));
    } catch (e) {
      toast("error", errorText(e));
    } finally {
      setBusyWhy(false);
    }
  }

  if (!pr) return <Loader2 className="mx-auto mt-10 size-5 animate-spin text-slate-500" />;
  const checks = pr.statusCheckRollup ?? [];
  return (
    <div className="space-y-4 p-5">
      <div>
        <h2 className="text-sm font-semibold text-slate-100">
          {pr.title} <span className="font-normal text-slate-500">#{pr.number}</span>
        </h2>
        <div className="mt-1 flex flex-wrap gap-2 text-label text-slate-500">
          <span>{pr.state.toLowerCase()}</span>
          <span>{pr.author?.login}</span>
          <span className="font-mono">
            {pr.headRefName} {"->"} {pr.baseRefName}
          </span>
          <span>mergeable: {pr.mergeable.toLowerCase()}</span>
          {pr.reviewDecision && <span>review: {pr.reviewDecision.toLowerCase().replace("_", " ")}</span>}
          <ExternalLink href={pr.url}>Open on GitHub</ExternalLink>
        </div>
      </div>
      <div className="flex flex-wrap gap-2">
        <Button
          size="sm"
          icon={<Bot className="size-3.5" />}
          onClick={() =>
            onAskHermes(`Review pull request #${pr.number} in ${repo} ("${pr.title}"). Read it with gh pr view ${pr.number} -R ${repo} and gh pr diff ${pr.number} -R ${repo}. List bugs, security issues and missing tests with file and line references, then suggest the review verdict. Do not submit the review yourself.`, localPath)
          }
          title="Hand this PR to Hermes in Chat Studio"
        >
          Ask Hermes to review
        </Button>
        <Button size="sm" variant="ghost" onClick={() => void studio.gh.prDiff(repo, number).then(setDiff).catch((e) => toast("error", errorText(e)))}>
          {diff === null ? "Show diff" : "Reload diff"}
        </Button>
      </div>
      {pr.body?.trim() && (
        <Card className="p-4">
          <Markdown text={pr.body} workingDir="" />
        </Card>
      )}
      {checks.length > 0 && (
        <div>
          <h3 className="mb-1.5 text-xs font-semibold text-slate-300">Checks</h3>
          <div className="space-y-1">
            {checks.map((c, i) => {
              const st = checkState(c);
              return (
                <div key={i} className="flex items-center gap-2 text-small text-slate-300">
                  {st === "ok" ? <CheckCircle2 className="size-3.5 text-ok" /> : st === "fail" ? <XCircle className="size-3.5 text-danger" /> : st === "pending" ? <Loader2 className="size-3.5 animate-spin text-amber-300" /> : <CircleSlash className="size-3.5 text-slate-500" />}
                  <span>{c.name ?? c.context}</span>
                  {(c.detailsUrl || c.targetUrl) && (
                    <ExternalLink href={c.detailsUrl ?? c.targetUrl} className="ml-auto text-caption">
                      details
                    </ExternalLink>
                  )}
                </div>
              );
            })}
          </div>
        </div>
      )}
      <div>
        <h3 className="mb-1.5 text-xs font-semibold text-slate-300">
          Files ({pr.files.length}) <span className="text-ok/80">+{pr.additions}</span> <span className="text-danger/80">-{pr.deletions}</span>
        </h3>
        <div className="max-h-48 overflow-y-auto rounded-lg border border-line font-mono text-label">
          {pr.files.map((f) => (
            <div key={f.path} className="flex gap-2 border-b border-line px-3 py-1 last:border-0">
              <span className="min-w-0 flex-1 truncate text-slate-300">{f.path}</span>
              <span className="text-ok/80">+{f.additions}</span>
              <span className="text-danger/80">-{f.deletions}</span>
            </div>
          ))}
        </div>
      </div>
      {diff !== null && (
        <div>
          <div className="mb-1 text-caption text-slate-500">
            Hover a new or unchanged line and press why to blame it in your local clone.
          </div>
          <div className="max-h-[480px] overflow-auto rounded-lg border border-line bg-canvas p-3 font-mono text-label leading-relaxed">
            {parseDiff(diff).map((r, i) => (
              <div key={i} className="group flex items-start">
                <div
                  className={`min-w-0 flex-1 whitespace-pre-wrap ${
                    r.text.startsWith("+") && !r.text.startsWith("+++")
                      ? "text-emerald-300"
                      : r.text.startsWith("-") && !r.text.startsWith("---")
                        ? "text-red-300"
                        : r.text.startsWith("@@")
                          ? "text-sky-300"
                          : "text-slate-400"
                  }`}
                >
                  {r.text || "\u00a0"}
                </div>
                {r.file !== undefined && r.line !== undefined && (
                  <button
                    onClick={() => void whyHere(r.file as string, r.line as number)}
                    disabled={!localPath || busyWhy}
                    title={localPath ? `Why is ${r.file}:${r.line} here?` : "Needs a local clone"}
                    className="invisible ml-2 shrink-0 rounded px-1 text-2xs text-sky-300 ring-1 ring-sky-500/30 hover:bg-sky-500/10 group-hover:visible disabled:opacity-40"
                  >
                    why
                  </button>
                )}
              </div>
            ))}
          </div>
        </div>
      )}
      {why && (
        <Modal title={`Why is ${why.file}:${why.line} here?`} onClose={() => setWhy(null)}>
          <ProvenanceCard p={why} />
        </Modal>
      )}
      <Thread items={[...pr.reviews, ...pr.comments]} />
      {pr.state === "OPEN" && (
        <>
          <Card className="space-y-2 p-4">
            <h3 className="text-xs font-semibold text-slate-300">Review</h3>
            <select value={review} onChange={(e) => setReview(e.target.value)} className="h-8 rounded-md border border-line bg-canvas px-2 text-xs text-slate-200" aria-label="Review type">
              <option value="comment">Comment</option>
              <option value="approve">Approve</option>
              <option value="request_changes">Request changes</option>
            </select>
            <CommentBox
              label="Submit review"
              placeholder="Review summary (Markdown)"
              onSubmit={(body) =>
                act((a) => guardedReview(repo, number, review, body, a)).then((ok) => {
                  if (ok) reload();
                  return ok;
                })
              }
            />
          </Card>
          <Card className="flex flex-wrap items-center gap-2 p-4">
            <h3 className="mr-2 text-xs font-semibold text-slate-300">Merge</h3>
            <select value={method} onChange={(e) => setMethod(e.target.value)} className="h-8 rounded-md border border-line bg-canvas px-2 text-xs text-slate-200" aria-label="Merge method">
              <option value="squash">Squash and merge</option>
              <option value="merge">Merge commit</option>
              <option value="rebase">Rebase and merge</option>
            </select>
            <label className="inline-flex items-center gap-1.5 text-xs text-slate-400">
              <input type="checkbox" checked={del} onChange={(e) => setDel(e.target.checked)} className="accent-emerald-500" /> Delete branch
            </label>
            <Button size="sm" variant="primary" icon={<GitMerge className="size-3.5" />} className="ml-auto" onClick={() => void act((a) => studio.gh.merge(repo, number, method, del, a)).then((ok) => ok && reload())} title="Asks for approval first">
              Merge
            </Button>
          </Card>
        </>
      )}
      <CommentBox placeholder="Add a comment" onSubmit={(body) => act((a) => studio.gh.comment(repo, "pr", number, body, a)).then((ok) => (ok && reload(), ok))} />
    </div>
  );
}

function guardedReview(repo: string, number: number, event: string, body: string, a: Approved) {
  return studio.gh.review(repo, number, event, body, a);
}

function IssueDetail({ repo, number, act, localPath, onAskHermes }: DetailProps & { number: number }) {
  const toast = useToast();
  const [issue, setIssue] = useState<GhIssueDetail | null>(null);
  const reload = useCallback(() => {
    studio.gh
      .issue(repo, number)
      .then(setIssue)
      .catch((e) => toast("error", errorText(e)));
  }, [number, repo, toast]);
  useEffect(() => {
    setIssue(null);
    reload();
  }, [reload]);
  if (!issue) return <Loader2 className="mx-auto mt-10 size-5 animate-spin text-slate-500" />;
  return (
    <div className="space-y-4 p-5">
      <div>
        <h2 className="text-sm font-semibold text-slate-100">
          {issue.title} <span className="font-normal text-slate-500">#{issue.number}</span>
        </h2>
        <div className="mt-1 flex gap-2 text-label text-slate-500">
          <span>{issue.state.toLowerCase()}</span>
          <span>{issue.author?.login}</span>
          <ExternalLink href={issue.url}>Open on GitHub</ExternalLink>
        </div>
      </div>
      <div className="flex flex-wrap gap-2">
        <Button
          size="sm"
          icon={<Bot className="size-3.5" />}
          onClick={() =>
            onAskHermes(`Work on issue #${issue.number} in ${repo}: "${issue.title}". Read it with gh issue view ${issue.number} -R ${repo} --comments, find the cause in this repository, then propose a fix on a new branch following the AgentOS constitution. Ask before pushing.`, localPath)
          }
          title="Hand this issue to Hermes in Chat Studio"
        >
          Ask Hermes to fix
        </Button>
        {issue.state === "OPEN" && (
          <>
            <Button size="sm" onClick={() => void act((a) => studio.gh.closeIssue(repo, number, "completed", a)).then((ok) => ok && reload())}>
              Close as completed
            </Button>
            <Button size="sm" variant="ghost" onClick={() => void act((a) => studio.gh.closeIssue(repo, number, "not planned", a)).then((ok) => ok && reload())}>
              Close as not planned
            </Button>
          </>
        )}
      </div>
      {issue.body?.trim() && (
        <Card className="p-4">
          <Markdown text={issue.body} workingDir="" />
        </Card>
      )}
      <Thread items={issue.comments} />
      <CommentBox placeholder="Add a comment" onSubmit={(body) => act((a) => studio.gh.comment(repo, "issue", number, body, a)).then((ok) => (ok && reload(), ok))} />
    </div>
  );
}

function RunDetail({ repo, id, act, localPath, onAskHermes }: DetailProps & { id: number }) {
  const toast = useToast();
  const [run, setRun] = useState<GhRunDetail | null>(null);
  const [log, setLog] = useState<string | null>(null);
  const [loadingLog, setLoadingLog] = useState(false);
  useEffect(() => {
    setRun(null);
    setLog(null);
    studio.gh
      .run(repo, id)
      .then(setRun)
      .catch((e) => toast("error", errorText(e)));
  }, [id, repo, toast]);
  if (!run) return <Loader2 className="mx-auto mt-10 size-5 animate-spin text-slate-500" />;
  const failed = run.conclusion && run.conclusion !== "success" && run.status === "completed";
  return (
    <div className="space-y-4 p-5">
      <div>
        <h2 className="flex items-center gap-2 text-sm font-semibold text-slate-100">
          <RunIcon status={run.status} conclusion={run.conclusion} /> {run.displayTitle}
        </h2>
        <div className="mt-1 flex gap-2 text-label text-slate-500">
          <span>{run.workflowName}</span>
          <span className="font-mono">{run.headBranch}</span>
          <span>{run.conclusion || run.status}</span>
          <ExternalLink href={run.url}>Open on GitHub</ExternalLink>
        </div>
      </div>
      <div className="flex flex-wrap gap-2">
        {failed && (
          <>
            <Button
              size="sm"
              icon={<Bot className="size-3.5" />}
              onClick={() =>
                onAskHermes(`GitHub Actions run ${id} (${run.workflowName}) failed in ${repo} on ${run.headBranch}. Read the failure with gh run view ${id} -R ${repo} --log-failed, find the root cause in this repository and propose the fix.`, localPath)
              }
            >
              Ask Hermes to diagnose
            </Button>
            <Button size="sm" onClick={() => void act((a) => studio.gh.rerun(repo, id, true, a))}>
              Re-run failed jobs
            </Button>
          </>
        )}
        {run.status === "completed" && (
          <Button size="sm" variant="ghost" onClick={() => void act((a) => studio.gh.rerun(repo, id, false, a))}>
            Re-run all jobs
          </Button>
        )}
        {failed && (
          <Button
            size="sm"
            variant="ghost"
            busy={loadingLog}
            onClick={() => {
              setLoadingLog(true);
              studio.gh
                .runLog(repo, id)
                .then(setLog)
                .catch((e) => toast("error", errorText(e)))
                .finally(() => setLoadingLog(false));
            }}
          >
            Failed log
          </Button>
        )}
      </div>
      <div className="space-y-2">
        {run.jobs.map((j) => (
          <Card key={j.name} className="p-3">
            <div className="flex items-center gap-2 text-xs font-medium text-slate-200">
              <RunIcon status={j.status} conclusion={j.conclusion} /> {j.name}
            </div>
            <div className="mt-1.5 space-y-0.5 pl-5">
              {j.steps.map((s) => (
                <div key={s.number} className="flex items-center gap-2 text-label text-slate-400">
                  <RunIcon status={s.status} conclusion={s.conclusion} /> {s.name}
                </div>
              ))}
            </div>
          </Card>
        ))}
      </div>
      {log !== null && <pre className="max-h-[480px] overflow-auto rounded-lg border border-line bg-canvas p-3 font-mono text-label leading-relaxed text-slate-300">{log || "No failed steps produced a log."}</pre>}
    </div>
  );
}

function CreateIssue({ repo, onClose, act }: { repo: string; onClose: () => void; act: Act }) {
  const [title, setTitle] = useState("");
  const [body, setBody] = useState("");
  return (
    <Modal
      title={`New issue in ${repo}`}
      onClose={onClose}
      footer={
        <>
          <Button variant="ghost" onClick={onClose}>
            Cancel
          </Button>
          <Button variant="primary" disabled={!title.trim()} onClick={() => void act((a) => studio.gh.createIssue(repo, title, body, a)).then((ok) => ok && onClose())}>
            Open issue
          </Button>
        </>
      }
    >
      <div className="space-y-3">
        <Field label="Title">
          <input autoFocus value={title} onChange={(e) => setTitle(e.target.value)} className={inputClass} />
        </Field>
        <Field label="Description" hint="Markdown">
          <textarea value={body} onChange={(e) => setBody(e.target.value)} rows={7} className={`${inputClass} resize-y`} />
        </Field>
      </div>
    </Modal>
  );
}

function CreatePr({ dir, onClose, act }: { dir: string; onClose: () => void; act: Act }) {
  const [title, setTitle] = useState("");
  const [body, setBody] = useState("");
  const [base, setBase] = useState("");
  const [draft, setDraft] = useState(false);
  return (
    <Modal
      title="New pull request"
      onClose={onClose}
      footer={
        <>
          <Button variant="ghost" onClick={onClose}>
            Cancel
          </Button>
          <Button variant="primary" disabled={!title.trim()} onClick={() => void act((a) => studio.gh.createPr(dir, title, body, base || null, null, draft, a)).then((ok) => ok && onClose())}>
            Open pull request
          </Button>
        </>
      }
    >
      <div className="space-y-3">
        <p className="text-label text-slate-500">From the current branch of {dir}. Push the branch first.</p>
        <Field label="Title">
          <input autoFocus value={title} onChange={(e) => setTitle(e.target.value)} placeholder="feat: ..." className={inputClass} />
        </Field>
        <Field label="Description" hint="Changes, assumptions and verification">
          <textarea value={body} onChange={(e) => setBody(e.target.value)} rows={7} className={`${inputClass} resize-y`} />
        </Field>
        <div className="flex items-end gap-3">
          <Field label="Base branch" hint="Empty uses the repository default">
            <input value={base} onChange={(e) => setBase(e.target.value)} placeholder="main" className={inputClass} />
          </Field>
          <label className="mb-6 inline-flex items-center gap-1.5 text-xs text-slate-400">
            <input type="checkbox" checked={draft} onChange={(e) => setDraft(e.target.checked)} className="accent-emerald-500" /> Draft
          </label>
        </div>
      </div>
    </Modal>
  );
}
