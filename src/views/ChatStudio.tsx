import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { ArrowUp, Brain, ChevronRight, Cloud, FolderOpen, GitPullRequest, Loader2, MessageSquarePlus, Paperclip, Plug, Sparkles, Square, SquareTerminal, Stethoscope, Workflow } from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState, type KeyboardEvent } from "react";
import { api, errorText, events, type HermesStatus, type MemoryEntry, type RepoInfo, type Skill } from "../lib/api";
import { studio, type Folder, type OrgSnapshot, type PlacedChat } from "../lib/studio";
import { ApprovalDeclined, useGuarded } from "../components/ApprovalProvider";
import { WorkspaceTree } from "../components/WorkspaceTree";
import { applyEvent, fromHistory, newId, type ChatItem } from "../lib/chat";
import { basename } from "../lib/format";
import { useCli } from "../components/CliDrawer";
import { Markdown } from "../components/Markdown";
import { ProviderSwitcher } from "../components/ProviderSwitcher";
import { ToolCard } from "../components/ToolCard";
import { UsageBar } from "../components/UsageBar";
import { Modal } from "../components/ui";
import { useToast } from "../components/Toasts";

type Backend = "hermes" | "claude-code";

const SLASH = [
  { cmd: "/doctor", label: "doctor", icon: Stethoscope, hint: "Run hermes doctor in the terminal" },
  { cmd: "/mcp-test", label: "mcp-test", icon: Plug, hint: "Ping every MCP server and list its tools" },
  { cmd: "/delegate-to-kanban", label: "delegate-to-kanban", icon: Workflow, hint: "Queue the text as a task on the Agent Bus" },
  { cmd: "/sync-notion", label: "sync-notion", icon: Cloud, hint: "Have Hermes update your AgentOS Command Center in Notion" },
  { cmd: "/gh", label: "gh", icon: GitPullRequest, hint: "Run a GitHub CLI command in the terminal, for example /gh pr list" },
] as const;

/** Same text as org::context_preamble in Rust: the agent reads attached files itself. */
function contextPreamble(files: string[]) {
  if (!files.length) return "";
  return `Attached context files (read the ones relevant to this message with your file tools):\n${files.map((f) => `- ${f}`).join("\n")}\n\n`;
}

const STARTERS = [
  "Summarize what changed in this repository this week and what is left to do.",
  "Call agentos-hub fetch_pending_tasks and plan how to clear the queue.",
  "Search shared memory for our architecture decisions and explain them briefly.",
  "Review the README in this folder and suggest the three most valuable fixes.",
];

const SYNC_NOTION_PROMPT =
  "Sync my AgentOS state to Notion. 1) Call agentos-hub fetch_pending_tasks with agent_name all and search_shared_memory for 'release'. 2) Using the Notion MCP tools, find my page titled 'AgentOS Command Center' and append a short dated 'Status sync' section listing open tasks (id, title, assignee) and the latest release notes. Do not remove existing content. 3) Reply with what you changed.";

function ls<T>(key: string, fallback: T): T {
  try {
    const v = localStorage.getItem(key);
    return v === null ? fallback : (JSON.parse(v) as T);
  } catch {
    return fallback;
  }
}

export type ChatIntent =
  | { kind: "open"; id: string; cwd: string; title: string; nonce: number }
  | { kind: "new"; cwd: string; projectSlug: string | null; parent: string | null; nonce: number };

export function ChatStudio({
  repos,
  hermes,
  home,
  intent,
  onStatusChanged,
  onOpenControl,
  incoming,
}: {
  repos: RepoInfo[];
  hermes: HermesStatus | null;
  home: string;
  /** Open a session, or start a branch with a known parent, from Projects. */
  intent: ChatIntent | null;
  onStatusChanged: () => void;
  onOpenControl: (tab: "providers" | "mcp") => void;
  /** A hand-off from another view: start a new chat with this draft (and folder). */
  incoming: { text: string; cwd?: string; nonce: number } | null;
}) {
  const toast = useToast();
  const cli = useCli();
  const guarded = useGuarded();
  const [snapshot, setSnapshot] = useState<OrgSnapshot | null>(null);
  const [query, setQuery] = useState("");
  const [loadingList, setLoadingList] = useState(false);
  const [current, setCurrent] = useState<string | null>(null);
  const [claudeSession, setClaudeSession] = useState<string | null>(null);
  const [items, setItems] = useState<ChatItem[]>([]);
  const [loadingHistory, setLoadingHistory] = useState(false);
  const [streaming, setStreaming] = useState(false);
  const [statusLine, setStatusLine] = useState("");
  const [backend, setBackend] = useState<Backend>(() => ls<Backend>("chat-backend", "hermes"));
  const [workingDir, setWorkingDir] = useState<string>(() => ls("chat-cwd", ""));
  const [draft, setDraft] = useState("");
  const [skills, setSkills] = useState<string[]>(() => ls<string[]>("chat-skills", []));
  const [skillList, setSkillList] = useState<Skill[] | null>(null);
  const [skillPicker, setSkillPicker] = useState(false);
  const [pendingBranch, setPendingBranch] = useState<{ parent: string; projectSlug: string | null; title: string } | null>(null);
  const pendingFolder = useRef<Folder | null>(null);
  const runRef = useRef<string | null>(null);
  const streamingRef = useRef(false);
  const scroller = useRef<HTMLDivElement>(null);
  const stick = useRef(true);
  const textarea = useRef<HTMLTextAreaElement>(null);

  const cwd = workingDir || home;
  useEffect(() => { localStorage.setItem("chat-backend", JSON.stringify(backend)); }, [backend]);
  useEffect(() => {
    localStorage.setItem("chat-cwd", JSON.stringify(workingDir));
    if (workingDir) void api.setWorkingDir(workingDir).catch(() => undefined);
  }, [workingDir]);

  // The stored folder is not trusted: a saved System32 value would bypass the resolver entirely.
  // Ask Rust on every mount and overwrite whatever the browser remembered.
  useEffect(() => {
    void api
      .workingDirDefault()
      .then((d) => {
        if (d.path && d.path !== workingDir) {
          setWorkingDir(d.path);
          if (d.forbidden_process_cwd && d.source === "home") {
            toast("info", `Started in ${basename(d.path)}; the saved folder was a system directory.`);
          }
        }
      })
      .catch(() => undefined);
    // First mount only.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);
  useEffect(() => { localStorage.setItem("chat-skills", JSON.stringify(skills)); }, [skills]);

  const loadSessions = useCallback(
    async (q = query) => {
      setLoadingList(true);
      try {
        setSnapshot(await studio.org.snapshot(q.trim().startsWith("#") ? null : q.trim() || null));
      } catch (e) {
        toast("error", errorText(e));
      } finally {
        setLoadingList(false);
      }
    },
    [query, toast],
  );

  useEffect(() => {
    const t = window.setTimeout(() => void loadSessions(query), 200);
    return () => window.clearTimeout(t);
  }, [query, loadSessions]);

  const backendRef = useRef(backend);
  backendRef.current = backend;
  const pendingRef = useRef(pendingBranch);
  pendingRef.current = pendingBranch;
  const [ledger, setLedger] = useState<MemoryEntry[]>([]);
  const pendingBranchRef = useRef(pendingBranch);
  pendingBranchRef.current = pendingBranch;
  const skillsRef = useRef(skills);
  skillsRef.current = skills;
  const reloadRef = useRef(loadSessions);
  reloadRef.current = loadSessions;

  useEffect(() => {
    void api.activeChat().then((id) => {
      if (id) {
        runRef.current = id;
        streamingRef.current = true;
        setStreaming(true);
        setStatusLine("A reply started earlier is still streaming");
      }
    });
  }, []);

  useEffect(() => {
    const un = events.chat((e) => {
      if (!streamingRef.current) return;
      if (runRef.current && e.run_id !== runRef.current) return;
      runRef.current = e.run_id;
      if ((e.kind === "status" || e.kind === "stderr") && e.text) setStatusLine(e.text);
      if (e.kind === "tool_start") setStatusLine(`Calling ${e.server ? `${e.server.replace(/_/g, "-")} / ` : ""}${e.tool_name}`);
      if (e.kind === "token") setStatusLine("Writing");
      if ((e.kind === "session" || e.kind === "complete") && e.session_id) {
        if (backendRef.current === "hermes") {
          setCurrent(e.session_id);
          const f = pendingFolder.current;
          if (f) {
            pendingFolder.current = null;
            void studio.org.placeChat(e.session_id, f.id, null).catch(() => undefined);
          }
          const branch = pendingBranchRef.current;
          if (branch) {
            pendingBranchRef.current = null;
            void api
              .linkBranch(branch.parent, branch.projectSlug, branch.title || "Branch", e.session_id)
              .catch(() => undefined);
            setPendingBranch(null);
          }
          void api.setChatSkills(e.session_id, skillsRef.current).catch(() => undefined);
        } else setClaudeSession(e.session_id);
      }
      setItems((prev) => applyEvent(prev, e));
      if (e.kind === "complete") {
        streamingRef.current = false;
        runRef.current = null;
        setStreaming(false);
        setStatusLine(e.stopped ? "Stopped" : e.duration_ms ? `Done in ${(e.duration_ms / 1000).toFixed(1)}s` : "Done");
        void reloadRef.current();
      }
    });
    return () => void un.then((f) => f());
  }, []);

  useEffect(() => {
    const el = scroller.current;
    if (el && stick.current) el.scrollTop = el.scrollHeight;
  }, [items]);

  useEffect(() => {
    const el = textarea.current;
    if (!el) return;
    el.style.height = "0px";
    el.style.height = `${Math.min(el.scrollHeight, 280)}px`;
  }, [draft]);

  async function openSession(s: PlacedChat) {
    if (streaming) return toast("info", "Wait for the reply to finish or press Stop");
    setCurrent(s.id);
    pendingFolder.current = null;
    setClaudeSession(null);
    setBackend("hermes");
    setLoadingHistory(true);
    setStatusLine("");
    try {
      setItems(fromHistory(await api.sessionMessages(s.id)));
      if (s.cwd && !workingDir) setWorkingDir(s.cwd);
      stick.current = true;
    } catch (e) {
      toast("error", errorText(e));
    } finally {
      setLoadingHistory(false);
    }
  }

  useEffect(() => {
    if (!incoming) return;
    if (streamingRef.current) {
      toast("info", "Wait for the reply to finish, then try again");
      return;
    }
    newChat();
    if (incoming.cwd) setWorkingDir(incoming.cwd);
    setDraft(incoming.text);
    window.setTimeout(() => textarea.current?.focus(), 30);
    // Only a new nonce starts a hand-off.
  }, [incoming?.nonce]);

  // Reload the ledger for this session shortly after a turn settles.
  useEffect(() => {
    if (!current) {
      setLedger([]);
      return;
    }
    const t = window.setTimeout(() => {
      void api.memoryLedger(current).then(setLedger).catch(() => undefined);
    }, 500);
    return () => window.clearTimeout(t);
  }, [current, items]);

  useEffect(() => {
    const onUse = (e: Event) => {
      const name = (e as CustomEvent<string>).detail;
      if (name) setSkills((cur) => (cur.includes(name) ? cur : [...cur, name]));
    };
    window.addEventListener("agentos:use-skill", onUse);
    return () => window.removeEventListener("agentos:use-skill", onUse);
  }, []);

  // Intents from Projects: open a session, or start a branch with a known parent.
  useEffect(() => {
    if (!intent) return;
    if (intent.kind === "open") {
      setWorkingDir(intent.cwd);
      setBackend("hermes");
      setCurrent(intent.id);
      setClaudeSession(null);
      setLoadingHistory(true);
      setStatusLine("");
      api
        .sessionMessages(intent.id)
        .then((m) => setItems(fromHistory(m)))
        .catch((e) => toast("error", errorText(e)))
        .finally(() => setLoadingHistory(false));
    } else {
      setWorkingDir(intent.cwd);
      setBackend("hermes");
      setCurrent(null);
      setClaudeSession(null);
      setItems([]);
      setStatusLine("");
      setPendingBranch(intent.parent ? { parent: intent.parent, projectSlug: intent.projectSlug, title: "" } : null);
      textarea.current?.focus();
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [intent?.nonce]);

  function newChat(folder: Folder | null = null) {
    if (streaming) return toast("info", "Wait for the reply to finish or press Stop");
    pendingFolder.current = folder;
    if (folder?.path) setWorkingDir(folder.path);
    setCurrent(null);
    setClaudeSession(null);
    setItems([]);
    setStatusLine("");
    textarea.current?.focus();
  }

  async function send(text: string) {
    const prompt = text.trim();
    if (!prompt || streaming) return;
    if (prompt.startsWith("/")) {
      const [cmd, ...rest] = prompt.split(/\s+/);
      if (SLASH.some((s) => s.cmd === cmd)) {
        setDraft("");
        return runSlash(cmd, rest.join(" "));
      }
    }
    setItems((prev) => [...prev, { kind: "user", id: newId("u"), text: prompt }]);
    setDraft("");
    // Files attached to this chat and its folders are listed for the agent, and shown here.
    let full = prompt;
    if (backend === "hermes") {
      try {
        const folderId = chats.find((c) => c.id === current)?.folder_id ?? pendingFolder.current?.id ?? null;
        const files = await studio.org.contextFiles(current, folderId);
        if (files.length) {
          full = contextPreamble(files) + prompt;
          setItems((prev) => [...prev, { kind: "notice", id: newId("n"), level: "info", text: `Attached for the agent: ${files.join(", ")}` }]);
        }
      } catch {
        // attachments are optional context
      }
    }
    stick.current = true;
    streamingRef.current = true;
    runRef.current = null;
    setStreaming(true);
    setStatusLine(backend === "hermes" ? "Starting Hermes" : "Starting Claude Code");
    try {
      const turn = items.filter((i) => i.kind === "user").length + 1;
      const started = await api.sendChat(backend === "hermes" ? current : claudeSession, full, cwd, backend, null, skills, turn);
      runRef.current = started.run_id;
    } catch (e) {
      streamingRef.current = false;
      setStreaming(false);
      setStatusLine("");
      setItems((prev) => [...prev, { kind: "notice", id: newId("n"), level: "error", text: errorText(e) }]);
    }
  }

  async function runSlash(cmd: string, arg: string) {
    const note = (level: "info" | "error" | "ok", text: string) => setItems((prev) => [...prev, { kind: "notice", id: newId("n"), level, text }]);
    try {
      if (cmd === "/doctor") {
        await cli.run("hermes doctor", cwd);
        note("info", "hermes doctor is running in the terminal below.");
      } else if (cmd === "/mcp-test") {
        note("info", "Testing every MCP server...");
        const servers = await api.mcpServers();
        const results = await Promise.all(servers.map((s) => api.testMcpServer(s.name).catch((e) => ({ name: s.name, ok: false, error: errorText(e), latency_ms: 0, tools: [], via: "native" as const }))));
        const lines = results.map((r) =>
          r.ok ? `- **${r.name}**: ${r.latency_ms} ms, ${r.tools.length} tools (${r.via === "native" ? "native Rust client" : "via Hermes"})` : `- **${r.name}**: failed, ${r.error}`,
        );
        setItems((prev) => [...prev, { kind: "assistant", id: newId("a"), text: `MCP check\n\n${lines.join("\n")}`, thinking: "", streaming: false }]);
      } else if (cmd === "/delegate-to-kanban") {
        const goal = arg || draft;
        if (!goal.trim()) return note("error", "Type the task after /delegate-to-kanban, for example: /delegate-to-kanban Audit the auth flow");
        const r = await api.dispatch(goal, workingDir || null, "auto", true);
        note("ok", `${r.task.id} queued on the Agent Bus for ${r.agent}. It is on the Mission Control board.`);
      } else if (cmd === "/sync-notion") {
        setBackend("hermes");
        await send(SYNC_NOTION_PROMPT);
      } else if (cmd === "/gh") {
        if (!arg.trim()) return note("info", "Type a GitHub CLI command after /gh, for example: /gh pr list, /gh run list, /gh issue view 12. The GitHub view has buttons for all of these.");
        await cli.run(`gh ${arg}`, cwd);
        note("info", `gh ${arg} is running in the terminal below.`);
      }
    } catch (e) {
      note("error", errorText(e));
    }
  }

  async function stop() {
    try {
      await api.stopChat();
      setStatusLine("Stopping");
    } catch (e) {
      toast("error", errorText(e));
    }
  }

  function onKey(e: KeyboardEvent<HTMLTextAreaElement>) {
    if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) {
      e.preventDefault();
      void send(draft);
    }
  }

  async function rename(id: string, title: string) {
    try {
      await api.renameSession(id, title);
    } catch (e) {
      toast("error", errorText(e));
    }
  }

  async function exportSession(id: string) {
    try {
      toast("ok", `Exported to ${await api.exportSession(id)}`);
    } catch (e) {
      toast("error", errorText(e));
    }
  }

  async function remove(id: string) {
    try {
      await guarded((a) => api.deleteSession(id, a.approved));
      toast("ok", "Session deleted");
      if (current === id) newChat();
      void loadSessions();
    } catch (e) {
      if (!(e instanceof ApprovalDeclined)) toast("error", errorText(e));
    }
  }

  async function continueInTerminal() {
    const sid = backend === "hermes" ? current : claudeSession;
    if (!sid) return toast("info", "Send a message first, then continue the conversation in a terminal");
    const t = await cli.openPty({ profile: backend === "hermes" ? "hermes-resume" : "claude-resume", session_id: sid, cwd });
    if (t) toast("ok", `${t.title} is open in the terminal panel. The chat stays here too.`);
  }

  async function browse() {
    const dir = await openDialog({ directory: true, title: "Working directory for this chat" });
    if (typeof dir === "string") setWorkingDir(dir);
  }

  const chats = useMemo(() => snapshot?.chats ?? [], [snapshot]);
  const currentChat = chats.find((c) => c.id === current);
  const title = useMemo(() => {
    if (backend === "claude-code") return claudeSession ? "Claude Code session" : "New Claude Code chat";
    return currentChat?.title ?? (current ? "Hermes session" : pendingFolder.current ? `New chat in ${pendingFolder.current.name}` : "New chat");
  }, [backend, claudeSession, current, currentChat]);
  const folderName = currentChat?.folder_id ? snapshot?.folders.find((f) => f.id === currentChat.folder_id)?.name : undefined;
  const attachedCount = currentChat
    ? (snapshot?.attachments ?? []).filter((a) => a.owner_kind === "chat" && a.owner_id === currentChat.id).length
    : 0;

  const repoOptions = useMemo(() => {
    const list = repos.map((r) => ({ path: r.path, name: r.name }));
    if (workingDir && !list.some((r) => r.path === workingDir)) list.unshift({ path: workingDir, name: basename(workingDir) });
    return list;
  }, [repos, workingDir]);

  return (
    <div className="flex h-full min-h-0">
      <aside className="flex w-72 shrink-0 flex-col border-r border-line bg-[#0a0e15]/60">
        <div className="space-y-2 p-3">
          <button
            onClick={() => newChat()}
            title="Start a new conversation (Ctrl+K, then New chat)"
            className="flex h-9 w-full items-center justify-center gap-2 rounded-lg bg-emerald-500 text-sm font-semibold text-emerald-950 hover:bg-emerald-400"
          >
            <MessageSquarePlus className="size-4" /> New Chat
          </button>
          <div className="flex items-center gap-1.5">
            <select
              value={workingDir}
              onChange={(e) => setWorkingDir(e.target.value)}
              className="h-8 min-w-0 flex-1 rounded-lg border border-line bg-card px-2 text-xs text-slate-300 outline-none"
              aria-label="Working directory"
              title={cwd}
            >
              <option value="">Home ({basename(home) || "~"})</option>
              {repoOptions.map((r) => (
                <option key={r.path} value={r.path}>
                  {r.name}
                </option>
              ))}
            </select>
            <button onClick={() => void browse()} title="Pick any folder" aria-label="Pick folder" className="grid size-8 place-items-center rounded-lg ring-1 ring-line text-slate-400 hover:bg-white/5 hover:text-slate-100">
              <FolderOpen className="size-3.5" />
            </button>
          </div>
        </div>
        <WorkspaceTree
          snapshot={snapshot}
          loading={loadingList}
          query={query}
          setQuery={setQuery}
          current={current}
          reload={() => void loadSessions()}
          actions={{
            onOpen: (c) => void openSession(c),
            onRename: rename,
            onExport: (id) => void exportSession(id),
            onDelete: remove,
            onNewChatIn: (f) => newChat(f),
          }}
        />
      </aside>

      <section className="flex min-w-0 flex-1 flex-col">
        <header className="flex h-14 shrink-0 items-center gap-3 border-b border-line px-5">
          <div className="min-w-0 flex-1">
            <div className="truncate text-sm font-semibold text-slate-100">{title}</div>
            <div className="flex items-center gap-1.5 truncate text-[11px] text-slate-500" title={cwd}>
              {folderName && <span className="rounded bg-white/5 px-1 text-slate-400">{folderName}</span>}
              {attachedCount > 0 && (
                <span className="inline-flex items-center gap-0.5 text-slate-400" title="Files attached to this chat are listed for the agent with every message">
                  <Paperclip className="size-3" />
                  {attachedCount}
                </span>
              )}
              <span className="truncate">{cwd}</span>
            </div>
          </div>
          <button
            onClick={() => {
              setSkillPicker(true);
              if (!skillList) void api.skills().then(setSkillList).catch((e) => toast("error", errorText(e)));
            }}
            title="Skills preloaded on the next turn"
            className={`inline-flex h-8 shrink-0 items-center gap-1.5 rounded-full px-3 text-[11.5px] ring-1 ${skills.length ? "bg-emerald-500/10 text-emerald-300 ring-emerald-500/30" : "bg-white/[0.04] text-slate-400 ring-line hover:text-slate-200"}`}
          >
            <Sparkles className="size-3.5" /> Skills {skills.length || ""}
          </button>
          {pendingBranch && (
            <span className="inline-flex h-8 shrink-0 items-center gap-1.5 rounded-full bg-sky-500/10 px-3 text-[11.5px] text-sky-300 ring-1 ring-sky-500/30" title="This chat will be recorded as a branch of another session">
              Branch
            </span>
          )}
          <div className="flex rounded-full bg-white/[0.04] p-0.5 ring-1 ring-line">
            {(["hermes", "claude-code"] as Backend[]).map((b) => (
              <button
                key={b}
                disabled={streaming}
                onClick={() => {
                  if (b === backend) return;
                  setBackend(b);
                  setCurrent(null);
                  setClaudeSession(null);
                  setItems([]);
                }}
                className={`h-7 rounded-full px-3 text-[11.5px] font-medium ${backend === b ? "bg-emerald-500/15 text-emerald-300" : "text-slate-400 hover:text-slate-200"}`}
              >
                {b === "hermes" ? "Hermes" : "Claude Code"}
              </button>
            ))}
          </div>
          <button
            onClick={() => void continueInTerminal()}
            disabled={streaming || !(backend === "hermes" ? current : claudeSession)}
            title={`Continue this conversation in a real terminal (${backend === "hermes" ? "hermes --resume" : "claude --resume"}), with live reasoning and typed approvals`}
            aria-label="Continue in terminal"
            className="inline-flex h-8 items-center gap-1.5 rounded-lg px-2.5 text-[11.5px] text-slate-300 ring-1 ring-line hover:bg-white/5 hover:text-slate-100 disabled:opacity-40"
          >
            <SquareTerminal className="size-3.5" /> Continue in terminal
          </button>
          <ProviderSwitcher backend={backend} hermes={hermes} onSwitched={onStatusChanged} onOpenControl={() => onOpenControl("providers")} />
        </header>

        <UsageBar sessionId={current} />
        <div
          ref={scroller}
          onScroll={(e) => {
            const el = e.currentTarget;
            stick.current = el.scrollHeight - el.scrollTop - el.clientHeight < 80;
          }}
          className="min-h-0 flex-1 overflow-y-auto"
        >
          <div className="mx-auto max-w-3xl px-6 py-6">
            {loadingHistory && <Loader2 className="mx-auto mt-10 size-5 animate-spin text-slate-500" />}
            {!loadingHistory && items.length === 0 && (
              <div className="mt-16 text-center">
                <div className="mx-auto mb-4 grid size-12 place-items-center rounded-2xl bg-emerald-500/10 ring-1 ring-emerald-500/25">
                  <Brain className="size-6 text-emerald-300" />
                </div>
                <h2 className="text-lg font-semibold text-slate-100">What should the fleet do?</h2>
                <p className="mt-1 text-xs text-slate-500">
                  {backend === "hermes" ? "Hermes" : "Claude Code"} works inside <span className="text-slate-300">{basename(cwd) || cwd}</span> with every MCP server you have connected.
                </p>
                <div className="mx-auto mt-6 grid max-w-2xl grid-cols-2 gap-2 text-left">
                  {STARTERS.map((s) => (
                    <button key={s} onClick={() => setDraft(s)} className="rounded-xl border border-line bg-card/60 px-3.5 py-3 text-xs leading-relaxed text-slate-300 hover:border-slate-600 hover:bg-card">
                      {s}
                    </button>
                  ))}
                </div>
              </div>
            )}
            {(() => {
              let turn = 0;
              return items.map((it) => {
              if (it.kind === "user") turn += 1;
              if (it.kind === "user")
                return (
                  <div key={it.id} className="my-4 flex justify-end">
                    <div className="max-w-[85%] whitespace-pre-wrap break-words rounded-2xl rounded-br-md bg-emerald-500/10 px-4 py-2.5 text-[13.5px] leading-relaxed text-slate-100 ring-1 ring-emerald-500/20">
                      {it.text}
                    </div>
                  </div>
                );
              if (it.kind === "assistant")
                return (
                  <div key={it.id} className="my-3">
                    {it.thinking && (
                      <details className="mb-2 rounded-lg bg-white/[0.02] px-3 py-1.5 text-xs text-slate-500 ring-1 ring-white/5">
                        <summary className="cursor-pointer select-none">
                          <ChevronRight className="mr-1 inline size-3" />
                          Thinking
                        </summary>
                        <div className="mt-1.5 whitespace-pre-wrap">{it.thinking}</div>
                      </details>
                    )}
                    {it.text.trim() && <Markdown text={it.text.replace(/^\s+/, "")} workingDir={cwd} />}
                    {it.streaming && <span className="ml-0.5 inline-block h-4 w-1.5 animate-pulse rounded-sm bg-emerald-400 align-middle" />}
                    <MemoryUsed entries={ledger.filter((e) => e.turn === turn)} />
                  </div>
                );
              if (it.kind === "tool") return <ToolCard key={it.id} tool={it} />;
              return (
                <div
                  key={it.id}
                  className={`my-3 whitespace-pre-wrap rounded-lg px-3 py-2 text-xs ring-1 ${
                    it.level === "error" ? "bg-red-500/10 text-red-200 ring-red-500/25" : it.level === "ok" ? "bg-emerald-500/10 text-emerald-200 ring-emerald-500/25" : "bg-white/[0.03] text-slate-400 ring-white/10"
                  }`}
                >
                  {it.text}
                </div>
              );
              });
            })()}
          </div>
        </div>

        <div className="shrink-0 px-6 pb-5">
          <div className="mx-auto max-w-3xl">
            <div className="mb-2 flex flex-wrap items-center gap-1.5">
              {SLASH.map((s) => (
                <button
                  key={s.cmd}
                  title={s.hint}
                  disabled={streaming}
                  onClick={() => void runSlash(s.cmd, s.cmd === "/delegate-to-kanban" ? draft : "").then(() => s.cmd === "/delegate-to-kanban" && setDraft(""))}
                  className="inline-flex h-6 items-center gap-1 rounded-full bg-white/[0.04] px-2.5 text-[11px] text-slate-400 ring-1 ring-white/5 hover:bg-white/[0.08] hover:text-slate-100 disabled:opacity-40"
                >
                  <s.icon className="size-3" />/{s.label}
                </button>
              ))}
              <span className="ml-auto truncate text-[11px] text-slate-500">
                {streaming && <Loader2 className="mr-1 inline size-3 animate-spin" />}
                {statusLine}
              </span>
            </div>
            <div className="flex items-end gap-2 rounded-2xl border border-line bg-card p-2 focus-within:border-emerald-500/50 focus-within:ring-2 focus-within:ring-emerald-500/10">
              <textarea
                ref={textarea}
                value={draft}
                onChange={(e) => setDraft(e.target.value)}
                onKeyDown={onKey}
                rows={1}
                placeholder={`Message ${backend === "hermes" ? "Hermes" : "Claude Code"} (Enter to send, Shift+Enter for a new line, / for commands)`}
                className="max-h-72 min-h-9 flex-1 resize-none bg-transparent px-2 py-2 text-[13.5px] leading-relaxed text-slate-100 outline-none placeholder:text-slate-600"
              />
              {streaming ? (
                <button onClick={() => void stop()} title="Stop" aria-label="Stop" className="grid size-9 shrink-0 place-items-center rounded-xl bg-red-500/15 text-red-300 ring-1 ring-red-500/30 hover:bg-red-500/25">
                  <Square className="size-4" />
                </button>
              ) : (
                <button
                  onClick={() => void send(draft)}
                  disabled={!draft.trim()}
                  title="Send"
                  aria-label="Send"
                  className="grid size-9 shrink-0 place-items-center rounded-xl bg-emerald-500 text-emerald-950 hover:bg-emerald-400 disabled:opacity-40"
                >
                  <ArrowUp className="size-4" />
                </button>
              )}
            </div>
          </div>
        </div>
      </section>
      {skillPicker && (
        <Modal
          title="Skills for this chat"
          onClose={() => setSkillPicker(false)}
          footer={
            <>
              <button onClick={() => setSkills([])} className="mr-auto text-[11.5px] text-slate-500 hover:text-slate-300">
                Clear all
              </button>
              <button onClick={() => setSkillPicker(false)} className="rounded-lg bg-emerald-500 px-4 py-2 text-sm font-semibold text-emerald-950 hover:bg-emerald-400">
                Done
              </button>
            </>
          }
        >
          <p className="mb-3 text-[11.5px] leading-relaxed text-slate-500">
            Selected skills are passed to Hermes as <span className="font-mono">-s</span> on every turn in this chat, so the agent starts with that knowledge loaded.
          </p>
          {!skillList && <Loader2 className="mx-auto size-4 animate-spin text-slate-500" />}
          <div className="max-h-72 space-y-0.5 overflow-y-auto">
            {(skillList ?? []).map((sk) => {
              const on = skills.includes(sk.name);
              return (
                <label key={sk.name} className="flex cursor-pointer items-start gap-2 rounded-lg px-2 py-1.5 hover:bg-white/[0.04]">
                  <input
                    type="checkbox"
                    checked={on}
                    onChange={(e) => setSkills((cur) => (e.target.checked ? [...cur, sk.name] : cur.filter((x) => x !== sk.name)))}
                    className="mt-0.5 accent-emerald-500"
                  />
                  <span className="min-w-0">
                    <span className="block text-[12.5px] text-slate-200">{sk.name}</span>
                    <span className="block line-clamp-1 text-[10.5px] text-slate-500">{sk.description || sk.category}</span>
                  </span>
                </label>
              );
            })}
          </div>
        </Modal>
      )}
    </div>
  );
}

/** What this turn read from and wrote to memory, from the tool events themselves. */
function MemoryUsed({ entries }: { entries: MemoryEntry[] }) {
  if (entries.length === 0) return null;
  const reads = entries.filter((e) => e.op === "read").length;
  const writes = entries.length - reads;
  return (
    <details className="mt-2 rounded-lg bg-white/[0.02] px-3 py-1.5 text-[11.5px] text-slate-500 ring-1 ring-white/5">
      <summary className="cursor-pointer select-none">
        <ChevronRight className="mr-1 inline size-3" />
        Memory used: {reads} read{reads === 1 ? "" : "s"}
        {writes > 0 ? `, ${writes} write${writes === 1 ? "" : "s"}` : ""}
      </summary>
      <ul className="mt-1.5 space-y-1">
        {entries.map((e, i) => (
          <li key={`${e.tool}-${i}`} className="flex gap-2">
            <span className={e.op === "write" ? "text-amber-300" : "text-sky-300"}>{e.op}</span>
            <span className="text-slate-400">{e.target}</span>
            <span className="truncate font-mono text-[11px] text-slate-300" title={e.path || e.tool}>
              {e.path || e.tool}
            </span>
          </li>
        ))}
      </ul>
    </details>
  );
}
