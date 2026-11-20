import type { ChatEvent, StoredMessage } from "./api";

export interface UserItem {
  kind: "user";
  id: string;
  text: string;
  /** Row id in Hermes state.db, present for messages loaded from history. */
  dbId?: number;
}

export interface AssistantItem {
  kind: "assistant";
  id: string;
  text: string;
  thinking: string;
  streaming: boolean;
  dbId?: number;
}

export interface ToolItem {
  kind: "tool";
  id: string;
  toolId: string;
  name: string;
  server: string | null;
  input: string;
  output: string | null;
  isError: boolean;
  durationMs: number | null;
  status: "running" | "done" | "error" | "unknown";
  dbId?: number;
}

export interface NoticeItem {
  kind: "notice";
  id: string;
  level: "info" | "error" | "ok";
  text: string;
}

export type ChatItem = UserItem | AssistantItem | ToolItem | NoticeItem;

let seq = 0;
export const newId = (p: string) => `${p}-${Date.now().toString(36)}-${(seq++).toString(36)}`;

/** `mcp__agentos_hub__fetch_pending_tasks` into server and tool parts. */
export function splitTool(full: string): { server: string | null; name: string } {
  if (full.startsWith("mcp__")) {
    const rest = full.slice(5);
    const i = rest.indexOf("__");
    if (i > 0) return { server: rest.slice(0, i), name: rest.slice(i + 2) };
  }
  return { server: null, name: full };
}

export function prettyJson(v: unknown): string {
  if (v === undefined || v === null) return "";
  if (typeof v === "string") {
    try {
      return JSON.stringify(JSON.parse(v), null, 2);
    } catch {
      return v;
    }
  }
  return JSON.stringify(v, null, 2);
}

function closeStreaming(items: ChatItem[]): ChatItem[] {
  const last = items[items.length - 1];
  if (last?.kind === "assistant" && last.streaming) {
    return [...items.slice(0, -1), { ...last, streaming: false }];
  }
  return items;
}

function closeTurn(items: ChatItem[]): ChatItem[] {
  return closeStreaming(items).map((item) => item.kind === "tool" && item.status === "running" ? { ...item, status: "unknown" } : item);
}

/** Fold one streamed event into the transcript. */
export function applyEvent(items: ChatItem[], e: ChatEvent): ChatItem[] {
  const last = items[items.length - 1];
  switch (e.kind) {
    case "token":
    case "thinking": {
      const chunk = e.text ?? "";
      if (!chunk) return items;
      const field = e.kind === "token" ? "text" : "thinking";
      if (last?.kind === "assistant" && last.streaming) {
        return [...items.slice(0, -1), { ...last, [field]: last[field] + chunk }];
      }
      const fresh: AssistantItem = { kind: "assistant", id: newId("a"), text: "", thinking: "", streaming: true };
      fresh[field] = chunk;
      return [...items, fresh];
    }
    case "tool_start": {
      const tool: ToolItem = {
        kind: "tool",
        id: newId("t"),
        toolId: e.tool_id ?? newId("call"),
        name: e.tool_name ?? "tool",
        server: e.server ?? null,
        input: prettyJson(e.input),
        output: null,
        isError: false,
        durationMs: null,
        status: "running",
      };
      return [...closeStreaming(items), tool];
    }
    case "tool_result": {
      let idx = -1;
      for (let i = items.length - 1; i >= 0; i--) {
        const it = items[i];
        if (it.kind === "tool" && (it.toolId === e.tool_id || (!e.tool_id && it.status === "running"))) {
          idx = i;
          break;
        }
      }
      const patch: Partial<ToolItem> & Pick<ToolItem, "output" | "isError" | "durationMs" | "status"> = {
        output: e.output ?? "",
        isError: Boolean(e.is_error),
        durationMs: e.duration_ms ?? null,
        status: (e.is_error ? "error" : "done") as ToolItem["status"],
      };
      if (e.input !== undefined && e.input !== null) patch.input = prettyJson(e.input);
      if (idx < 0) {
        return [
          ...items,
          { kind: "tool", id: newId("t"), toolId: e.tool_id ?? "", name: e.tool_name ?? "tool", server: e.server ?? null, input: "", ...patch },
        ];
      }
      const next = items.slice();
      next[idx] = { ...(next[idx] as ToolItem), ...patch };
      return next;
    }
    case "error":
      return [...closeTurn(items), { kind: "notice", id: newId("n"), level: "error", text: e.text ?? "The agent reported an error" }];
    case "complete": {
      let next = closeTurn(items);
      const lastUser = next.map((i) => i.kind).lastIndexOf("user");
      const answered = next.slice(lastUser + 1).some((i) => i.kind === "assistant" && i.text.trim());
      if (!answered && e.text?.trim() && !e.stopped) {
        next = [...next, { kind: "assistant", id: newId("a"), text: e.text, thinking: "", streaming: false }];
      }
      if (e.stopped) next = [...next, { kind: "notice", id: newId("n"), level: "info", text: "Stopped. The partial reply is kept above." }];
      return next;
    }
    default:
      return items;
  }
}

/** Rebuild a transcript from Hermes's stored messages. */
export function fromHistory(messages: StoredMessage[]): ChatItem[] {
  const items: ChatItem[] = [];
  const byCall = new Map<string, number>();
  for (const m of messages) {
    if (m.role === "user" && m.content.trim()) {
      items.push({ kind: "user", id: `u-${m.id}`, text: m.content, dbId: m.id });
    } else if (m.role === "assistant") {
      if (m.content.trim()) items.push({ kind: "assistant", id: `a-${m.id}`, text: m.content, thinking: "", streaming: false, dbId: m.id });
      for (const c of m.tool_calls) {
        const { server, name } = splitTool(c.name);
        byCall.set(c.id, items.length);
        items.push({
          kind: "tool",
          id: `t-${m.id}-${c.id}`,
          toolId: c.id,
          name,
          server,
          input: prettyJson(c.arguments),
          output: null,
          isError: false,
          durationMs: null,
          status: "unknown",
          dbId: m.id,
        });
      }
    } else if (m.role === "tool") {
      const idx = m.tool_call_id ? byCall.get(m.tool_call_id) : undefined;
      if (idx !== undefined) {
        const t = items[idx] as ToolItem;
        items[idx] = { ...t, output: m.content, status: "done" };
      }
    }
  }
  return items;
}

/**
 * The state.db message id to fork at for the item at `index`.
 *
 * Items loaded from history carry their row id. Items streamed in this view do not, so they are
 * matched by turn: a user item maps to the same-numbered user message, and an assistant or tool
 * item maps to the last stored message of its turn (fork after the whole reply).
 */
export function forkPoint(items: ChatItem[], index: number, stored: StoredMessage[]): number | null {
  const it = items[index];
  if (!it || it.kind === "notice") return null;
  if (it.dbId !== undefined && stored.some((m) => m.id === it.dbId)) return it.dbId;
  const turn = items.slice(0, index + 1).filter((i) => i.kind === "user").length;
  // Positions in stored (display) order, not ids, so a reordered row cannot shift the turn.
  const userAt = stored.flatMap((m, i) => (m.role === "user" && m.content.trim() ? [i] : []));
  if (it.kind === "user") return turn > 0 && userAt[turn - 1] !== undefined ? stored[userAt[turn - 1]].id : null;
  const from = turn === 0 ? 0 : userAt[turn - 1];
  if (from === undefined) return null;
  const to = userAt[turn] ?? stored.length;
  for (let i = to - 1; i >= from; i--) if (stored[i].role !== "user") return stored[i].id;
  return null;
}

export interface SourceEvidence {
  kind: "file" | "web";
  value: string;
  tool: ToolItem;
}

export interface AgentEvidence {
  label: string;
  task: string;
  background: boolean;
  tool: ToolItem;
}

export function chatEvidence(items: ChatItem[]) {
  const tools = items.filter((item): item is ToolItem => item.kind === "tool");
  const sources: SourceEvidence[] = [];
  const agents: AgentEvidence[] = [];
  for (const tool of tools) {
    let input: Record<string, unknown> = {};
    try {
      const parsed: unknown = JSON.parse(tool.input);
      if (parsed && typeof parsed === "object" && !Array.isArray(parsed)) input = parsed as Record<string, unknown>;
    } catch {
      continue;
    }
    let toolName = tool.name;
    if (Array.isArray(input.calls) && input.calls.length === 1) {
      const call = input.calls[0];
      if (call && typeof call === "object" && !Array.isArray(call)
        && typeof call.name === "string" && call.name.trim()
        && call.arguments && typeof call.arguments === "object" && !Array.isArray(call.arguments)) {
        const innerName = splitTool(call.name).name;
        if (toolName === "tool_call" || toolName === innerName) {
          toolName = innerName;
          input = call.arguments;
        }
      }
    }
    const name = toolName.toLowerCase().replace(/[- ]/g, "_");
    const fileRead = ["read", "read_file", "read_files", "file_read"].includes(name);
    const webRead = ["webfetch", "web_fetch", "web_read", "fetch_url", "browse_url"].includes(name);
    // ACP reports the files a call touched as `path` or `path:line`; only a read tool turns them
    // into sources, so a shell command that lists a path does not claim to have read it.
    const locations = fileRead && Array.isArray(input.locations)
      ? input.locations.filter((value): value is string => typeof value === "string").map((value) => value.replace(/:\d+$/, ""))
      : [];
    const values = fileRead
      ? [input.file_path, input.path, ...(Array.isArray(input.paths) ? input.paths : []), ...(Array.isArray(input.files) ? input.files : []), ...locations]
      : webRead ? [input.url, ...(Array.isArray(input.urls) ? input.urls : [])] : [];
    const seen = new Set<string>();
    for (const value of values) {
      if (typeof value !== "string" || !value.trim() || /[\r\n\0]/.test(value)) continue;
      let source = value.trim();
      if (webRead) {
        try {
          const url = new URL(source);
          if (url.protocol !== "https:" && url.protocol !== "http:") continue;
          source = url.origin + url.pathname;
        } catch {
          continue;
        }
      }
      if (seen.has(source)) continue;
      seen.add(source);
      sources.push({ kind: fileRead ? "file" : "web", value: source, tool });
    }
    if (["agent", "task", "delegate_task", "spawn_agent", "delegate_to_subagent"].includes(name)) {
      const label = [input.subagent_type, input.agent_name, input.agent, input.target_agent].find((value) => typeof value === "string" && value.trim());
      const task = [input.description, input.title, input.task, input.prompt].find((value) => typeof value === "string" && value.trim());
      agents.push({ label: typeof label === "string" ? label : "Delegated agent", task: typeof task === "string" ? task : tool.name, background: input.run_in_background === true, tool });
    }
  }
  return { tools, sources, agents, running: tools.filter((tool) => tool.status === "running").length };
}

export function relativeTime(epochSeconds: number): string {
  if (!epochSeconds) return "";
  const diff = Date.now() / 1000 - epochSeconds;
  if (diff < 60) return "just now";
  if (diff < 3600) return `${Math.floor(diff / 60)}m ago`;
  if (diff < 86400) return `${Math.floor(diff / 3600)}h ago`;
  if (diff < 86400 * 7) return `${Math.floor(diff / 86400)}d ago`;
  return new Date(epochSeconds * 1000).toLocaleDateString();
}
