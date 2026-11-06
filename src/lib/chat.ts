import type { ChatEvent, StoredMessage } from "./api";

export interface UserItem {
  kind: "user";
  id: string;
  text: string;
}

export interface AssistantItem {
  kind: "assistant";
  id: string;
  text: string;
  thinking: string;
  streaming: boolean;
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
  status: "running" | "done" | "error";
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
      const patch = {
        output: e.output ?? "",
        isError: Boolean(e.is_error),
        durationMs: e.duration_ms ?? null,
        status: (e.is_error ? "error" : "done") as ToolItem["status"],
      };
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
      return [...closeStreaming(items), { kind: "notice", id: newId("n"), level: "error", text: e.text ?? "The agent reported an error" }];
    case "complete": {
      let next = closeStreaming(items).map((it) => (it.kind === "tool" && it.status === "running" ? { ...it, status: "done" as const } : it));
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
      items.push({ kind: "user", id: `u-${m.id}`, text: m.content });
    } else if (m.role === "assistant") {
      if (m.content.trim()) items.push({ kind: "assistant", id: `a-${m.id}`, text: m.content, thinking: "", streaming: false });
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
          status: "done",
        });
      }
    } else if (m.role === "tool") {
      const idx = m.tool_call_id ? byCall.get(m.tool_call_id) : undefined;
      if (idx !== undefined) {
        const t = items[idx] as ToolItem;
        items[idx] = { ...t, output: m.content };
      }
    }
  }
  return items;
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
