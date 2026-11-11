// Text statistics, an official AgentOS Studio MCP bundle.
// K-ONE IT SOLUTIONS, Kone Tshivhinda. MIT licence.
//
// A stdio MCP server (newline-delimited JSON-RPC 2.0) with two read-only tools. Studio starts it
// only through `agentos-studio --extension-mcp solutions.k-one.text-stats`, which verifies the
// pack, clears the environment and runs it in a sandbox folder.
import { createInterface } from "node:readline";

const TOOLS = [
  {
    name: "count_words",
    description: "Count the words in a text.",
    inputSchema: { type: "object", properties: { text: { type: "string" } }, required: ["text"] },
  },
  {
    name: "count_lines",
    description: "Count the lines in a text.",
    inputSchema: { type: "object", properties: { text: { type: "string" } }, required: ["text"] },
  },
];

function reply(id, result) {
  process.stdout.write(JSON.stringify({ jsonrpc: "2.0", id, result }) + "\n");
}
function fail(id, code, message) {
  process.stdout.write(JSON.stringify({ jsonrpc: "2.0", id, error: { code, message } }) + "\n");
}

function call(name, args) {
  const text = typeof args?.text === "string" ? args.text : "";
  if (name === "count_words") return String(text.split(/\s+/).filter(Boolean).length);
  if (name === "count_lines") return String(text === "" ? 0 : text.split(/\r?\n/).length);
  return null;
}

createInterface({ input: process.stdin }).on("line", (line) => {
  let msg;
  try {
    msg = JSON.parse(line);
  } catch {
    return;
  }
  const { id, method, params } = msg;
  if (id === undefined) return; // notifications need no answer
  if (method === "initialize") {
    reply(id, {
      protocolVersion: params?.protocolVersion ?? "2024-11-05",
      capabilities: { tools: {} },
      serverInfo: { name: "text-stats", version: "1.0.0" },
    });
  } else if (method === "tools/list") {
    reply(id, { tools: TOOLS });
  } else if (method === "tools/call") {
    const out = call(params?.name, params?.arguments);
    if (out === null) fail(id, -32602, `unknown tool ${params?.name}`);
    else reply(id, { content: [{ type: "text", text: out }] });
  } else {
    fail(id, -32601, `method ${method} not found`);
  }
});
