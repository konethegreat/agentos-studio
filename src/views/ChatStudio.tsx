import { open as openDialog } from "@tauri-apps/plugin-dialog";
import {
  ArrowUp,
  BookDown,
  Brain,
  ChevronRight,
  FolderOpen,
  Loader2,
  MessageSquarePlus,
  Pencil,
  Search,
  Square,
  Stethoscope,
  Trash2,
  Workflow,
  Plug,
  Cloud,
} from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState, type KeyboardEvent } from "react";
import { api, errorText, events, type HermesStatus, type RepoInfo, type SessionSummary } from "../lib/api";
import { applyEvent, fromHistory, newId, relativeTime, type ChatItem } from "../lib/chat";
import { basename } from "../lib/format";
import { useCli } from "../components/CliDrawer";
import { Markdown } from "../components/Markdown";
import { ProviderSwitcher } from "../components/ProviderSwitcher";
import { ToolCard } from "../components/ToolCard";
import { useToast } from "../components/Toasts";

type Backend = "hermes" | "claude-code";

const SLASH = [
  { cmd: "/doctor", label: "doctor", icon: Stethoscope, hint: "Run hermes doctor in the terminal" },
  { cmd: "/mcp-test", label: "mcp-test", icon: Plug, hint: "Ping every MCP server and list its tools" },
  { cmd: "/delegate-to-kanban", label: "delegate-to-kanban", icon: Workflow, hint: "Queue the text as a task on the Agent Bus" },
  { cmd: "/sync-notion", label: "sync-notion", icon: Cloud, hint: "Have Hermes update your AgentOS Command Center in Notion" },
] as const;

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

export function ChatStudio({
  repos,
  hermes,
  home,
  onStatusChanged,
  onOpenControl,
}: {
  repos: RepoInfo[];
  hermes: HermesStatus | null;
  home: string;
  onStatusChanged: () => void;
  onOpenControl: (tab: "providers" | "mcp") => void;
}) {
  const toast = useToast();
  const cli = useCli();
  const [sessions, setSessions] = useState<SessionSummary[]>([]);
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
  const [editing, setEditing] = useState<{ id: string; title: string } | null>(null);
  const [confirmDelete, setConfirmDelete] = useState<string | null>(null);
  const runRef = useRef<string | null>(null);
  const streamingRef = useRef(false);
  const scroller = useRef<HTMLDivElement>(null);
  const stick = useRef(true);
  const textarea = useRef<HTMLTextAreaElement>(null);

  const cwd = workingDir || home;
  useEffect(() => localStorage.setItem("chat-backend", JSON.stringify(backend)), [backend]);
  useEffect(() => localStorage.setItem("chat-cwd", JSON.stringify(workingDir)), [workingDir]);

  const loadSessions = useCallback(
    async (q = query) => {
      setLoadingList(true);
      try {
        setSessions(await api.sessions(q.trim() || null));
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
        if (backendRef.current === "hermes") setCurrent(e.session_id);
        else setClaudeSession(e.session_id);
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

  async function openSession(s: SessionSummary) {
    if (streaming) return toast("info", "Wait for the reply to finish or press Stop");
    setCurrent(s.id);
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

  function newChat() {
    if (streaming) return toast("info", "Wait for the reply to finish or press Stop");
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
    stick.current = true;
    streamingRef.current = true;
    runRef.current = null;
    setStreaming(true);
    setStatusLine(backend === "hermes" ? "Starting Hermes" : "Starting Claude Code");
    try {
      const started = await api.sendChat(backend === "hermes" ? current : claudeSession, prompt, cwd, backend, null);
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

  async function saveRename() {
    if (!editing) return;
    try {
      await api.renameSession(editing.id, editing.title);
      setEditing(null);
      void loadSessions();
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
    if (confirmDelete !== id) {
      setConfirmDelete(id);
      window.setTimeout(() => setConfirmDelete((c) => (c === id ? null : c)), 4000);
      return;
    }
    try {
      await api.deleteSession(id);
      toast("ok", "Session deleted");
      if (current === id) newChat();
      setConfirmDelete(null);
      void loadSessions();
    } catch (e) {
      toast("error", errorText(e));
    }
  }

  async function browse() {
    const dir = await openDialog({ directory: true, title: "Working directory for this chat" });
    if (typeof dir === "string") setWorkingDir(dir);
  }

  const title = useMemo(() => {
    if (backend === "claude-code") return claudeSession ? "Claude Code session" : "New Claude Code chat";
    return sessions.find((s) => s.id === current)?.title ?? (current ? "Hermes session" : "New chat");
  }, [backend, claudeSession, current, sessions]);

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
            onClick={newChat}
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
          <div className="flex items-center gap-2 rounded-lg bg-card px-2 ring-1 ring-line">
            <Search className="size-3.5 text-slate-500" />
            <input value={query} onChange={(e) => setQuery(e.target.value)} placeholder="Search conversations" className="h-8 min-w-0 flex-1 bg-transparent text-xs text-slate-100 outline-none placeholder:text-slate-600" />
            {loadingList && <Loader2 className="size-3.5 animate-spin text-slate-500" />}
          </div>
        </div>
        <div className="min-h-0 flex-1 overflow-y-auto px-2 pb-3">
          {sessions.length === 0 && !loadingList && <div className="px-3 py-6 text-center text-xs text-slate-600">{query ? "No conversations match" : "No Hermes conversations yet"}</div>}
          {sessions.map((s) => (
            <div
              key={s.id}
              className={`group mb-0.5 rounded-lg px-2.5 py-2 ${current === s.id ? "bg-white/[0.07]" : "hover:bg-white/[0.03]"}`}
            >
              {editing?.id === s.id ? (
                <input
                  autoFocus
                  value={editing.title}
                  onChange={(e) => setEditing({ id: s.id, title: e.target.value })}
                  onKeyDown={(e) => {
                    if (e.key === "Enter") void saveRename();
                    if (e.key === "Escape") setEditing(null);
                  }}
                  onBlur={() => void saveRename()}
                  className="w-full rounded-md bg-canvas px-1.5 py-0.5 text-xs text-slate-100 outline-none ring-1 ring-emerald-500/50"
                />
              ) : (
                <button onClick={() => void openSession(s)} className="block w-full text-left">
                  <div className="truncate text-[12.5px] text-slate-200">{s.title}</div>
                  <div className="mt-0.5 flex items-center gap-1.5 text-[10.5px] text-slate-500">
                    <span>{relativeTime(s.last_activity)}</span>
                    <span>{s.message_count} msgs</span>
                    {s.source && s.source !== "cli" && <span className="truncate rounded bg-white/5 px-1">{s.source}</span>}
                  </div>
                </button>
              )}
              <div className="mt-1 hidden items-center gap-0.5 group-hover:flex">
                <button onClick={() => setEditing({ id: s.id, title: s.title })} title="Rename" aria-label="Rename" className="rounded p-1 text-slate-500 hover:bg-white/5 hover:text-slate-200">
                  <Pencil className="size-3" />
                </button>
                <button onClick={() => void exportSession(s.id)} title="Export to Obsidian" aria-label="Export to Obsidian" className="rounded p-1 text-slate-500 hover:bg-white/5 hover:text-slate-200">
                  <BookDown className="size-3" />
                </button>
                <button
                  onClick={() => void remove(s.id)}
                  title={confirmDelete === s.id ? "Click again to delete" : "Delete"}
                  className={`ml-auto inline-flex items-center gap-1 rounded p-1 text-[10.5px] ${confirmDelete === s.id ? "bg-red-500/15 text-red-300" : "text-slate-500 hover:bg-white/5 hover:text-red-300"}`}
                >
                  <Trash2 className="size-3" />
                  {confirmDelete === s.id && "Delete"}
                </button>
              </div>
            </div>
          ))}
        </div>
      </aside>

      <section className="flex min-w-0 flex-1 flex-col">
        <header className="flex h-14 shrink-0 items-center gap-3 border-b border-line px-5">
          <div className="min-w-0 flex-1">
            <div className="truncate text-sm font-semibold text-slate-100">{title}</div>
            <div className="truncate text-[11px] text-slate-500" title={cwd}>
              {cwd}
            </div>
          </div>
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
          <ProviderSwitcher backend={backend} hermes={hermes} onSwitched={onStatusChanged} onOpenControl={() => onOpenControl("providers")} />
        </header>

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
            {items.map((it) => {
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
            })}
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
    </div>
  );
}
