import { test } from "node:test";
import assert from "node:assert/strict";
import { applyEvent, chatEvidence, forkPoint, fromHistory, type ChatItem, type ToolItem } from "../src/lib/chat.ts";
import type { ChatEvent, StoredMessage } from "../src/lib/api.ts";

const event = (kind: ChatEvent["kind"], fields: Partial<ChatEvent> = {}): ChatEvent => ({ run_id: "fixture-run", kind, ...fields });
const message = (id: number, role: string, content: string, fields: Partial<StoredMessage> = {}): StoredMessage => ({
  id, role, content, tool_calls: [], tool_name: null, tool_call_id: null, timestamp: 1_800_000_000, ...fields,
});
const start = (id: string): ChatEvent => event("tool_start", { tool_id: id, tool_name: "Read", input: { file_path: "src/main.rs" } });
const tools = (items: ChatItem[]): ToolItem[] => items.filter((item): item is ToolItem => item.kind === "tool");

for (const terminal of [event("complete"), event("complete", { stopped: true }), event("error", { text: "Connection closed" })]) {
  test(`${terminal.kind}${terminal.stopped ? " stopped" : ""} does not invent successful tool results`, () => {
    const started = applyEvent([], start("call-1"));
    const ended = tools(applyEvent(started, terminal));
    assert.equal(ended[0].status, "unknown");
    assert.equal(ended[0].output, null);
    assert.equal(tools(started)[0].status, "running");
  });
}

test("late results resolve only the matching unfinished tool", () => {
  let items = applyEvent(applyEvent([], start("one")), start("two"));
  items = applyEvent(items, event("complete"));
  items = applyEvent(items, event("tool_result", { tool_id: "one", output: "Read complete", duration_ms: 34 }));
  assert.deepEqual(tools(items).map((tool) => [tool.toolId, tool.status, tool.output]), [["one", "done", "Read complete"], ["two", "unknown", null]]);
  assert.equal(tools(items)[0].durationMs, 34);
});

test("reported errors survive reply completion", () => {
  let items = applyEvent([], start("failed"));
  items = applyEvent(items, event("tool_result", { tool_id: "failed", output: "Access denied", is_error: true }));
  const tool = tools(applyEvent(items, event("complete")))[0];
  assert.equal(tool.status, "error");
  assert.equal(tool.isError, true);
  assert.equal(tool.output, "Access denied");
});

test("history distinguishes missing results from recorded empty results", () => {
  const stored = [message(1, "assistant", "", { tool_calls: [
    { id: "missing", name: "Read", arguments: "{}" },
    { id: "empty", name: "mcp__fixture__read", arguments: "{}" },
  ] }), message(2, "tool", "", { tool_call_id: "empty", tool_name: "read" })];
  const restored = tools(fromHistory(stored));
  assert.deepEqual(restored.map((tool) => [tool.toolId, tool.status, tool.output]), [["missing", "unknown", null], ["empty", "done", ""]]);
  assert.equal(restored[1].server, "fixture");
});

test("source evidence links structured read inputs to their actual tool result", () => {
  let items = applyEvent([], start("read"));
  items = applyEvent(items, event("tool_result", { tool_id: "read", output: "File contents" }));
  items = applyEvent(items, event("tool_start", { tool_id: "web", tool_name: "WebFetch", input: { url: "https://docs.example.test/guide?token=private#section" } }));
  const evidence = chatEvidence(items);
  assert.deepEqual(evidence.sources.map((source) => [source.kind, source.value, source.tool.status]), [["file", "src/main.rs", "done"], ["web", "https://docs.example.test/guide", "running"]]);
  assert.equal(evidence.sources[0].tool.output, "File contents");
  assert.equal(evidence.running, 1);
});

test("arbitrary prose and write inputs are not reported as source reads", () => {
  let items = applyEvent([], event("token", { text: "I will read src/future.rs and https://example.test" }));
  items = applyEvent(items, event("tool_start", { tool_id: "write", tool_name: "Write", input: { file_path: "src/new.rs" } }));
  items = applyEvent(items, event("tool_start", { tool_id: "callback", tool_name: "configure", input: { url: "https://example.test/hook" } }));
  items = applyEvent(items, event("tool_start", { tool_id: "invalid", tool_name: "WebFetch", input: { url: "javascript:alert(1)" } }));
  assert.deepEqual(chatEvidence(items).sources, []);
});

test("multi-file reads deduplicate per call without losing separate attempts", () => {
  let items = applyEvent([], event("tool_start", { tool_id: "many", tool_name: "read_files", input: { paths: ["a.rs", "a.rs", "b.rs", 7] } }));
  items = applyEvent(items, event("tool_result", { tool_id: "many", output: "Denied", is_error: true }));
  items = applyEvent(items, event("tool_start", { tool_id: "retry", tool_name: "Read", input: { file_path: "a.rs" } }));
  assert.deepEqual(chatEvidence(items).sources.map((source) => [source.value, source.tool.status]), [["a.rs", "error"], ["b.rs", "error"], ["a.rs", "running"]]);
});

test("delegations expose recorded calls without inventing agent liveness", () => {
  let items = applyEvent([], event("tool_start", { tool_id: "agent", tool_name: "Agent", input: { subagent_type: "Explore", description: "Trace imports", run_in_background: true } }));
  items = applyEvent(items, event("tool_result", { tool_id: "agent", output: "Task dispatched" }));
  const delegated = chatEvidence(items).agents;
  assert.equal(delegated.length, 1);
  assert.equal(delegated[0].label, "Explore");
  assert.equal(delegated[0].task, "Trace imports");
  assert.equal(delegated[0].background, true);
  assert.equal(delegated[0].tool.status, "done");
  assert.equal(chatEvidence(items).running, 0);
});

test("malformed inputs retain activity without guessed sources or agent names", () => {
  const items = applyEvent([], event("tool_start", { tool_id: "bad", tool_name: "Read", input: "not JSON: src/main.rs" }));
  const evidence = chatEvidence(items);
  assert.equal(evidence.tools.length, 1);
  assert.equal(evidence.sources.length, 0);
  assert.equal(evidence.agents.length, 0);
});

test("single-call history wrappers preserve source and delegation evidence", () => {
  const stored = [message(20, "assistant", "", { tool_calls: [
    { id: "file", name: "tool_call", arguments: JSON.stringify({ calls: [{ name: "mcp__files__read_file", arguments: { path: "src/main.rs" } }] }) },
    { id: "agent", name: "tool_call", arguments: JSON.stringify({ calls: [{ name: "delegate_task", arguments: { agent: "reviewer", task: "Check imports", run_in_background: true } }] }) },
  ] }), message(21, "tool", "File contents", { tool_call_id: "file" })];
  const restored = fromHistory(stored);
  const evidence = chatEvidence(restored);
  assert.deepEqual(evidence.sources.map((source) => [source.value, source.tool.toolId, source.tool.output]), [["src/main.rs", "file", "File contents"]]);
  assert.deepEqual(evidence.agents.map((agent) => [agent.label, agent.task, agent.background, agent.tool.status]), [["reviewer", "Check imports", true, "unknown"]]);
  assert.equal(evidence.sources[0].tool, tools(restored)[0]);
  assert.equal(forkPoint(restored, 1, stored), 20);
});

test("unrelated tools and malformed wrappers do not imply an inner call ran", () => {
  const items = fromHistory([message(30, "assistant", "", { tool_calls: [
    { id: "config", name: "configure", arguments: JSON.stringify({ calls: [{ name: "read_file", arguments: { path: "not-read.rs" } }] }) },
    { id: "invalid", name: "tool_call", arguments: JSON.stringify({ calls: [{ name: 7, arguments: { path: "not-read.rs" } }] }) },
    { id: "batch", name: "tool_call", arguments: JSON.stringify({ calls: [{ name: "read_file", arguments: { path: "one.rs" } }, { name: "read_file", arguments: { path: "two.rs" } }] }) },
  ] })]);
  assert.equal(chatEvidence(items).tools.length, 3);
  assert.deepEqual(chatEvidence(items).sources, []);
});

test("late ACP results add arguments and locations to the same tool card", () => {
  let items = applyEvent([], event("tool_start", { tool_id: "read", tool_name: "read_file" }));
  items = applyEvent(items, event("tool_result", { tool_id: "read", output: "ok", input: { path: "src/lib.rs", locations: ["src/lib.rs:12"] } }));
  const tool = tools(items)[0];
  assert.deepEqual(JSON.parse(tool.input), { path: "src/lib.rs", locations: ["src/lib.rs:12"] });
  assert.deepEqual(chatEvidence(items).sources.map((source) => [source.kind, source.value]), [["file", "src/lib.rs"]]);
});

test("locations on a non-read call do not become source evidence", () => {
  const items = applyEvent([], event("tool_start", { tool_id: "sh", tool_name: "shell", input: { command: "cat x", locations: ["a.rs:1"] } }));
  assert.deepEqual(chatEvidence(items).sources, []);
});

test("tool evidence changes preserve the recorded fork point", () => {
  const stored = [message(8, "user", "Read the source"), message(11, "assistant", "", { tool_calls: [{ id: "call", name: "Read", arguments: "{}" }] })];
  const restored = fromHistory(stored);
  assert.equal(forkPoint(restored, 1, stored), 11);
  assert.equal(forkPoint(restored, 0, stored), 8);
});
