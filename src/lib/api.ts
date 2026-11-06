import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export type AgentName = "hermes" | "claude-code" | "qoder" | "antigravity" | "human";
export type TaskState = "pending" | "in_progress" | "completed";

export interface Task {
  id: string;
  title: string;
  status: string;
  state: TaskState;
  assigned_to: string;
  creator: string;
  project: string;
  priority: string;
  timestamp: string;
  claimed_by: string | null;
  claimed_at: string | null;
  completed_by: string | null;
  completed_at: string | null;
  context_files: string[];
  path: string;
  body: string;
}

export interface Board {
  pending: Task[];
  in_progress: Task[];
  completed: Task[];
}

export interface WorkerInfo {
  task_id: string;
  agent: string;
  pid: number;
  started_at: string;
  log_path: string;
}

export interface WorkerEvent {
  task_id: string;
  stream: "stdout" | "stderr" | "system" | "exit";
  line: string;
  at: string;
  code?: number | null;
}

export interface Component {
  id: string;
  label: string;
  pillar: number;
  ok: boolean;
  detail: string;
  action: string;
}

export interface Pillar {
  number: number;
  title: string;
  ok: boolean;
}

export interface StackStatus {
  components: Component[];
  pillars: Pillar[];
  connected: number;
  total: number;
  vault: string;
  studio_binary: string;
  checked_at: string;
}

export interface SetupEvent {
  job: string;
  component: string;
  level: "cmd" | "out" | "err" | "step" | "ok" | "error";
  line: string;
  done: boolean;
  ok?: boolean;
}

export interface RepoInfo {
  name: string;
  path: string;
  stack: string[];
  branch: string;
  dirty_files: number;
  remote: string;
  status: "active" | "recent" | "dormant" | "no commits";
  last_commit: string;
  rules_synced: boolean;
}

export interface SyncResult {
  path: string;
  name: string;
  actions: [string, string][];
  commit: string;
  skipped: string[];
  error: string | null;
}

export interface Law {
  number: number;
  title: string;
  enabled: boolean;
}

export interface ConstitutionPayload {
  constitution: string;
  system_context: string;
  laws: Law[];
  path: string;
}

export interface BroadcastReport {
  dashes_replaced: boolean;
  vault: [string, string][];
  global: [string, string][];
  repos: SyncResult[];
}

export interface NoteInfo {
  relative_path: string;
  title: string;
  modified: string;
  size: number;
}

export interface SearchHit {
  path: string;
  relative_path: string;
  score: number;
  matched_terms: string[];
  matches: { line: number; snippet: string }[];
}

export interface SettingsView {
  vault: string;
  scan_roots: string[];
  studio_binary: string;
  version: string;
  home: string;
}

export interface DispatchResult {
  task: { id: string; path: string; assigned_to: string };
  worker: WorkerInfo | null;
  agent: string;
}

export interface ProjectResult {
  path: string;
  github: string | null;
  note: string;
  notion_task: { id: string };
  steps: string[];
}


export interface HermesStatus {
  installed: boolean;
  hermes_path: string | null;
  version: string | null;
  claude_path: string | null;
  claude_version: string | null;
  provider: string;
  provider_id: string | null;
  provider_label: string;
  model: string;
  mcp_total: number;
  mcp_enabled: number;
  sessions: number;
  home: string;
  config_ok: boolean;
  error: string | null;
  path_healed: number;
  checked_at: string;
}

export interface ModelConfig {
  provider: string;
  model: string;
  base_url: string | null;
}

export interface ProviderView {
  id: string;
  label: string;
  hermes_provider: string;
  kind: "subscription" | "api_key" | "oauth" | "local";
  key_env: string[];
  key_set: boolean;
  ready: boolean;
  active: boolean;
  detail: string;
  blurb: string;
  default_model: string;
}

export interface ProviderConfig {
  active: ModelConfig;
  active_id: string | null;
  active_label: string;
  providers: ProviderView[];
  config_path: string;
  env_path: string;
}

export interface ProviderTest {
  ok: boolean;
  latency_ms: number;
  reply: string;
  error: string | null;
}

export interface SessionSummary {
  id: string;
  title: string;
  source: string;
  model: string;
  provider: string;
  started_at: number;
  last_activity: number;
  message_count: number;
  tool_call_count: number;
  cwd: string;
  repo_root: string;
  cost_usd: number;
  preview: string;
  pinned: boolean;
}

export interface ToolCallView {
  id: string;
  name: string;
  arguments: string;
}

export interface StoredMessage {
  id: number;
  role: string;
  content: string;
  tool_calls: ToolCallView[];
  tool_name: string | null;
  tool_call_id: string | null;
  timestamp: number;
}

export interface AgentReg {
  agent: string;
  enabled: boolean;
  matches_primary: boolean;
}

export interface McpServer {
  name: string;
  transport: "stdio" | "http";
  command: string | null;
  args: string[];
  url: string | null;
  env_keys: string[];
  auth: string | null;
  agents: AgentReg[];
  is_agentos_hub: boolean;
  legacy_python: boolean;
}

export interface ToolSchema {
  name: string;
  description: string;
  input_schema: unknown;
}

export interface ProbeResult {
  name: string;
  ok: boolean;
  via: "native" | "hermes";
  latency_ms: number;
  server_name: string;
  server_version: string;
  protocol: string;
  tools: ToolSchema[];
  error: string | null;
}

export interface AgentOutcome {
  agent: string;
  ok: boolean;
  detail: string;
}

export interface CatalogParam {
  key: string;
  label: string;
  placeholder: string;
  default: string;
  secret: boolean;
  env: boolean;
  required: boolean;
}

export interface CatalogEntry {
  id: string;
  name: string;
  description: string;
  transport: "stdio" | "http";
  command_or_url: string;
  args: string[];
  auth: string | null;
  params: CatalogParam[];
  requires: string;
}

export type ChatEventKind = "status" | "session" | "token" | "thinking" | "tool_start" | "tool_result" | "stderr" | "error" | "complete";

export interface ChatEvent {
  run_id: string;
  kind: ChatEventKind;
  text?: string;
  tool_id?: string;
  tool_name?: string;
  server?: string;
  input?: unknown;
  output?: string;
  is_error?: boolean;
  duration_ms?: number;
  session_id?: string;
  tokens?: Record<string, number>;
  cost_usd?: number;
  exit_code?: number;
  stopped?: boolean;
}

export interface ChatStarted {
  run_id: string;
  backend: string;
  working_dir: string;
  pid: number;
}

export interface CliLine {
  run_id: string;
  stream: "start" | "stdout" | "stderr" | "exit";
  line: string;
  code?: number;
  duration_ms?: number;
}

export const MCP_AGENTS = ["hermes", "claude-code", "qoder", "antigravity"] as const;


export type BootStatus = "ok" | "repaired" | "warn" | "failed" | "skipped";

export interface BootStep {
  id: string;
  label: string;
  status: BootStatus;
  detail: string;
  ms: number;
  fix_view: string | null;
}

export interface BootSummary {
  ok: number;
  repaired: number;
  warn: number;
  failed: number;
  skipped: number;
}

export interface BootReport {
  app_version: string;
  started_at: string;
  elapsed_ms: number;
  ready: boolean;
  steps: BootStep[];
  summary: BootSummary;
  logs_dir: string;
  vault: string;
  previous_crash: string | null;
}

export interface BootProgress {
  steps: BootStep[];
  done: boolean;
  report?: BootReport;
}

export const api = {
  getSettings: () => invoke<SettingsView>("get_settings"),
  saveSettings: (vault: string, scanRoots: string[]) => invoke<SettingsView>("save_settings", { vault, scanRoots }),
  status: () => invoke<StackStatus>("check_full_stack_status"),
  repair: (component: string) => invoke<string>("install_or_repair_component", { component }),
  scaffoldVault: (path: string) => invoke<string[]>("scaffold_vault", { path }),
  board: () => invoke<Board>("list_board"),
  readTask: (taskId: string) => invoke<{ task: Task; markdown: string; log: string }>("read_task", { taskId }),
  dispatch: (prompt: string, projectPath: string | null, targetAgent: string, queueOnly = false) =>
    invoke<DispatchResult>("quick_mission_dispatch", { prompt, projectPath, targetAgent, queueOnly }),
  runNow: (taskId: string, agent: string) => invoke<WorkerInfo>("run_task_now", { taskId, agent }),
  moveTask: (taskId: string, to: TaskState) => invoke<string>("move_task", { taskId, to }),
  stopWorker: (taskId: string) => invoke<string>("stop_worker", { taskId }),
  workers: () => invoke<WorkerInfo[]>("list_workers"),
  scanFleet: () => invoke<RepoInfo[]>("scan_fleet"),
  syncFleet: (repoPaths: string[], autoCommitRules: boolean) =>
    invoke<SyncResult[]>("sync_fleet_rules", { repoPaths, autoCommitRules }),
  createProject: (name: string, parentDir: string, stackTemplate: string, createGithubRepo: boolean, privateRepo: boolean) =>
    invoke<ProjectResult>("create_new_project_from_ui", { name, parentDir, stackTemplate, createGithubRepo, privateRepo }),
  loadConstitution: () => invoke<ConstitutionPayload>("load_constitution"),
  toggleLaw: (constitutionText: string, number: number, enabled: boolean) =>
    invoke<{ constitution: string; laws: Law[] }>("toggle_law", { constitutionText, number, enabled }),
  broadcast: (constitutionText: string, systemContext: string, includeRepos: boolean) =>
    invoke<BroadcastReport>("save_and_broadcast", { constitutionText, systemContext, includeRepos }),
  memoryNotes: () => invoke<NoteInfo[]>("list_memory_notes"),
  readNote: (relativePath: string) => invoke<string>("read_note", { relativePath }),
  searchMemory: (query: string) => invoke<SearchHit[]>("search_memory", { query }),

  hermesStatus: (refresh = false) => invoke<HermesStatus>("get_hermes_status", { refresh }),
  providerConfig: () => invoke<ProviderConfig>("get_provider_config"),
  setProvider: (provider: string, model: string, apiKey: string | null, baseUrl: string | null) =>
    invoke<{ config: ProviderConfig; log: string[] }>("set_active_provider", { provider, model, apiKey, baseUrl }),
  models: (provider: string) => invoke<string[]>("list_available_models", { provider }),
  testProvider: (provider: string, model: string, baseUrl: string | null) =>
    invoke<ProviderTest>("test_provider", { provider, model, baseUrl }),
  loginProvider: (provider: string) => invoke<string>("login_provider", { provider }),
  sessions: (query: string | null, limit = 300) => invoke<SessionSummary[]>("list_hermes_sessions", { query, limit }),
  sessionMessages: (sessionId: string) => invoke<StoredMessage[]>("load_session_messages", { sessionId }),
  renameSession: (sessionId: string, title: string) => invoke<void>("rename_session", { sessionId, title }),
  deleteSession: (sessionId: string, approved?: boolean) => invoke<void>("delete_session", { sessionId, approved }),
  exportSession: (sessionId: string) => invoke<string>("export_session_to_obsidian", { sessionId }),

  mcpServers: () => invoke<McpServer[]>("get_all_mcp_servers"),
  addMcpServer: (a: {
    name: string;
    transport: string;
    commandOrUrl: string;
    args: string[];
    envVars: Record<string, string>;
    syncToAllAgents: boolean;
    auth?: string | null;
    agents?: string[] | null;
  }) => invoke<AgentOutcome[]>("add_mcp_server", a),
  removeMcpServer: (name: string) => invoke<AgentOutcome[]>("remove_mcp_server", { name }),
  testMcpServer: (name: string) => invoke<ProbeResult>("test_mcp_server", { name }),
  mcpOauth: (name: string) => invoke<string>("trigger_mcp_oauth", { name }),
  setMcpEnabled: (name: string, enabled: boolean) => invoke<string>("set_mcp_enabled", { name, enabled }),
  syncMcpServer: (name: string, agents: string[]) => invoke<AgentOutcome[]>("sync_mcp_server", { name, agents }),
  cutoverHub: () => invoke<AgentOutcome[]>("cutover_agentos_hub"),
  mcpCatalog: () => invoke<CatalogEntry[]>("mcp_catalog"),
  installCatalog: (id: string, values: Record<string, string>, syncToAllAgents: boolean) =>
    invoke<AgentOutcome[]>("install_catalog_server", { id, values, syncToAllAgents }),
  ghToken: () => invoke<string>("gh_token"),

  sendChat: (sessionId: string | null, prompt: string, workingDir: string, agentBackend: string, model: string | null) =>
    invoke<ChatStarted>("send_chat_message", { sessionId, prompt, workingDir, agentBackend, model }),
  stopChat: () => invoke<string>("stop_active_chat"),
  activeChat: () => invoke<string | null>("active_chat"),
  applyCode: (workingDir: string, relPath: string, content: string) =>
    invoke<{ path: string; created: boolean; bytes: number }>("apply_code_to_file", { workingDir, relPath, content }),
  bootSnapshot: () => invoke<BootReport | null>("boot_snapshot"),
  bootRun: () => invoke<BootReport>("boot_run"),
  bootLogsDir: () => invoke<string>("boot_logs_dir"),
  bootReadLog: (name: string) => invoke<string>("boot_read_log", { name }),
  bootOpenLogs: () => invoke<string>("boot_open_logs"),

  runCli: (command: string, cwd: string) => invoke<string>("execute_cli_command", { command, cwd }),
  killCli: (runId: string) => invoke<void>("kill_cli_command", { runId }),
};

export const events = {
  vaultChanged: (cb: (p: { paths: string[]; memory: boolean }) => void): Promise<UnlistenFn> =>
    listen<{ paths: string[]; memory: boolean }>("agentos://vault-changed", (e) => cb(e.payload)),
  worker: (cb: (e: WorkerEvent) => void): Promise<UnlistenFn> =>
    listen<WorkerEvent>("agentos://worker-stream", (e) => cb(e.payload)),
  setup: (cb: (e: SetupEvent) => void): Promise<UnlistenFn> =>
    listen<SetupEvent>("agentos://setup-progress", (e) => cb(e.payload)),
  hermesStatus: (cb: (s: HermesStatus) => void): Promise<UnlistenFn> =>
    listen<HermesStatus>("agentos://hermes-status", (e) => cb(e.payload)),
  chat: (cb: (e: ChatEvent) => void): Promise<UnlistenFn> => listen<ChatEvent>("agentos://chat-event", (e) => cb(e.payload)),
  cli: (cb: (e: CliLine) => void): Promise<UnlistenFn> => listen<CliLine>("agentos://cli-output", (e) => cb(e.payload)),
  boot: (cb: (p: BootProgress) => void): Promise<UnlistenFn> => listen<BootProgress>("agentos://boot", (e) => cb(e.payload)),
};

export function errorText(e: unknown): string {
  if (typeof e === "string") return e;
  if (e instanceof Error) return e.message;
  return JSON.stringify(e);
}
