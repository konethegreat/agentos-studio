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

/** A chat branched at one message into a new Hermes session. */
export interface ForkResult {
  /** The new session, or null when the fork point was the first message (nothing to carry). */
  session_id: string | null;
  parent_id: string;
  title: string;
  cwd: string;
  turns: number;
  /** The forked-at user message, returned to the composer for editing. */
  draft: string | null;
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
  /** agentos-hub only: some agent runs a hub binary other than this app version's. */
  hub_outdated: boolean;
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

export interface Mission {
  id: string;
  repo: string;
  goal: string;
  acceptance: string;
  role: string;
  agent: string;
  status: "queued" | "running" | "review" | "blocked" | "done" | "failed";
  provider: string;
  model: string;
  budget_usd: number;
  spent_usd: number;
  task_id: string | null;
  reviewer: string | null;
  review_verdict: "approve" | "reject" | null;
  review_note: string | null;
  pr_url: string | null;
  created_at: number;
  updated_at: number;
}

export interface MemoryEntry {
  session_id: string;
  turn: number;
  op: "read" | "write";
  target: "vault" | "notion" | "file";
  path: string;
  tool: string;
  at: number;
}

export interface MemoryNote {
  path: string;
  title: string;
  kind: string;
  status: string;
  repo: string | null;
  in_degree: number;
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


export interface ProjectFolder {
  path: string;
  label: string | null;
  primary: boolean;
}

export interface Project {
  id: string;
  slug: string;
  name: string;
  description: string | null;
  icon: string | null;
  color: string | null;
  primary_path: string | null;
  archived: boolean;
  folders: ProjectFolder[];
  session_count: number;
  last_activity: number;
}

export interface ProjectSession {
  id: string;
  title: string;
  cwd: string;
  started_at: number;
  last_activity: number;
  message_count: number;
  parent_session_id: string | null;
  branch_title: string | null;
}

export interface Skill {
  name: string;
  description: string;
  category: string;
  version: string | null;
  tags: string[];
  path: string;
  bytes: number;
}

export interface HubSkill {
  name: string;
  identifier: string;
  source: string;
  trust_level: string;
  description: string;
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

  sendChat: (sessionId: string | null, prompt: string, workingDir: string, agentBackend: string, model: string | null, skills: string[] = [], turn?: number) =>
    invoke<ChatStarted>("send_chat_message", { sessionId, prompt, workingDir, agentBackend, model, skills, turn: turn ?? null }),
  memoryLedger: (sessionId: string, limit = 400) => invoke<MemoryEntry[]>("memory_ledger", { sessionId, limit }),
  memoryRecent: (limit = 50) => invoke<MemoryEntry[]>("memory_recent", { limit }),
  memoryIndexNotes: () => invoke<MemoryNote[]>("memory_notes"),
  stopChat: () => invoke<string>("stop_active_chat"),
  activeChat: () => invoke<string | null>("active_chat"),
  applyCode: (workingDir: string, relPath: string, content: string) =>
    invoke<{ path: string; created: boolean; bytes: number }>("apply_code_to_file", { workingDir, relPath, content }),
  projects: () => invoke<Project[]>("list_projects"),
  projectSessions: (slug: string) => invoke<ProjectSession[]>("project_sessions", { slug }),
  createHermesProject: (name: string, folders: string[]) => invoke<string>("create_project", { name, folders }),
  renameProject: (slug: string, name: string) => invoke<string>("rename_project", { slug, name }),
  archiveProject: (slug: string, archived: boolean) => invoke<string>("archive_project", { slug, archived }),
  addProjectFolder: (slug: string, path: string) => invoke<string>("add_project_folder", { slug, path }),
  linkBranch: (parent: string, projectSlug: string | null, title: string, child: string) =>
    invoke<void>("link_branch", { parent, projectSlug, title, child }),
  forkSession: (sessionId: string, atMessageId: number) => invoke<ForkResult>("fork_session", { sessionId, atMessageId }),
  chatSkills: (sessionId: string) => invoke<string[]>("get_chat_skills", { sessionId }),
  setChatSkills: (sessionId: string, skills: string[]) => invoke<void>("set_chat_skills", { sessionId, skills }),

  workingDirDefault: () => invoke<{ path: string; source: string; forbidden_process_cwd: boolean }>("working_dir_default"),
  missions: () => invoke<Mission[]>("list_missions", { limit: 100 }),
  studioUpdateCheck: () => invoke<string | null>("studio_update_check"),
  studioUpdateInstall: () => invoke<string>("studio_update_install"),

  sessionUsage: (sessionId: string) =>
    invoke<{
      model: string | null;
      input_tokens: number;
      output_tokens: number;
      cost_usd: number;
      messages: number;
      compacted_messages: number;
      context_limit: number | null;
      context_used_pct: number | null;
      context_estimated: boolean;
    } | null>("session_usage", { sessionId }),
  providerCredits: () =>
    invoke<{ provider: string; kind: string; remaining_usd: number | null; used_usd: number | null; note: string }[]>("provider_credits"),

  providerConnections: () => invoke<{ id: string; label: string; connected: boolean; detail: string; needs: string; env_var: string | null }[]>("provider_connections"),
  saveProviderKey: (provider: string, key: string) =>
    invoke<string>("save_provider_key", { provider, key }),

  toolPolicies: () => invoke<{ server: string; tool: string; policy: string }[]>("tool_policies"),
  setToolPolicy: (server: string, tool: string, policy: string) =>
    invoke<void>("set_tool_policy", { server, tool, policy }),
  hubTools: () => invoke<string[]>("hub_tools"),

  offlineStatus: () =>
    invoke<{ online: boolean; local_models: string[]; note: string }>("offline_status"),

  helpDocs: () => invoke<{ title: string; path: string; body: string }[]>("help_docs"),

  routingPolicy: (repo: string) =>
    invoke<{ roles: Record<string, { provider: string; model: string }>; allow_merge: boolean } | null>("routing_policy", { repo }),

  routerSuggestions: () =>
    invoke<{ role: string; provider: string; model: string; attempts: number; approvals: number; score: number; avg_cost_usd: number; reason: string }[]>("router_suggestions"),

  modelCatalog: () =>
    invoke<{
      providers: { id: string; label: string; models: string[]; active: boolean }[];
      active_provider: string | null;
      active_model: string | null;
    }>("model_catalog"),

  notionStatus: () =>
    invoke<{ connected: boolean; has_parent: boolean; token_var: string }>("notion_status"),
  notionPublish: (dryRun = false) =>
    invoke<{ connected: boolean; dry_run: boolean; pages: { title: string; hash: string; action: string }[]; error: string | null }>(
      "notion_publish",
      { dryRun },
    ),
  notionPullInbox: () => invoke<number>("notion_pull_inbox"),
  automationStatus: () => invoke<{ enabled: boolean; max_concurrent: number }>("automation_status"),
  setAutomation: (enabled: boolean, maxConcurrent?: number) =>
    invoke<{ enabled: boolean; max_concurrent: number }>("set_automation", {
      enabled,
      maxConcurrent: maxConcurrent ?? null,
    }),
  automationTick: () => invoke<{ dispatched: string[]; moved_to_review: string[]; running: number }>("automation_tick"),
  createMission: (input: {
    repo: string;
    goal: string;
    acceptance: string;
    role: string;
    agent: string;
    provider: string;
    model: string;
    budgetUsd: number;
  }) =>
    invoke<Mission>("create_mission", {
      repo: input.repo,
      goal: input.goal,
      acceptance: input.acceptance,
      role: input.role,
      agent: input.agent,
      provider: input.provider,
      model: input.model,
      budgetUsd: input.budgetUsd,
    }),
  dispatchMission: (id: string) => invoke<Mission>("dispatch_mission", { id }),
  reviewWithAgent: (id: string, reviewer = "claude-code") =>
    invoke<Mission>("review_with_agent", { id, reviewer }),
  mergeMission: (id: string, allowlisted: boolean) =>
    invoke<string>("merge_mission", { id, allowlisted }),
  reviewMission: (id: string, reviewer: string, verdict: "approve" | "reject", note: string) =>
    invoke<Mission>("review_mission", { id, reviewer, verdict, note }),
  missionBlockers: (id: string, ciGreen: boolean, allowlisted: boolean) =>
    invoke<string[]>("mission_merge_blockers", { id, ciGreen, allowlisted }),

  ciBadge: (dir?: string) => invoke<number>("ci_badge", { dir: dir ?? null }),

  getPref: (key: string) => invoke<string | null>("get_pref", { key }),
  setPref: (key: string, value: string) => invoke<void>("set_pref", { key, value }),
  setWorkingDir: (path: string) => invoke<string>("set_working_dir", { path }),

  skills: () => invoke<Skill[]>("list_skills"),
  readSkill: (name: string) => invoke<string>("read_skill", { name }),
  searchSkills: (query: string, limit = 15) => invoke<HubSkill[]>("search_skills", { query, limit }),
  installSkill: (identifier: string) => invoke<string>("install_skill", { identifier }),
  removeSkill: (name: string) => invoke<string>("remove_skill", { name }),
  updateSkills: () => invoke<string>("update_skills"),

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
