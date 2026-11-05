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

export const api = {
  getSettings: () => invoke<SettingsView>("get_settings"),
  saveSettings: (vault: string, scanRoots: string[]) => invoke<SettingsView>("save_settings", { vault, scanRoots }),
  status: () => invoke<StackStatus>("check_full_stack_status"),
  repair: (component: string) => invoke<string>("install_or_repair_component", { component }),
  scaffoldVault: (path: string) => invoke<string[]>("scaffold_vault", { path }),
  board: () => invoke<Board>("list_board"),
  readTask: (taskId: string) => invoke<{ task: Task; markdown: string; log: string }>("read_task", { taskId }),
  dispatch: (prompt: string, projectPath: string | null, targetAgent: string) =>
    invoke<DispatchResult>("quick_mission_dispatch", { prompt, projectPath, targetAgent }),
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
};

export const events = {
  vaultChanged: (cb: (p: { paths: string[]; memory: boolean }) => void): Promise<UnlistenFn> =>
    listen<{ paths: string[]; memory: boolean }>("agentos://vault-changed", (e) => cb(e.payload)),
  worker: (cb: (e: WorkerEvent) => void): Promise<UnlistenFn> =>
    listen<WorkerEvent>("agentos://worker-stream", (e) => cb(e.payload)),
  setup: (cb: (e: SetupEvent) => void): Promise<UnlistenFn> =>
    listen<SetupEvent>("agentos://setup-progress", (e) => cb(e.payload)),
};

export function errorText(e: unknown): string {
  if (typeof e === "string") return e;
  if (e instanceof Error) return e.message;
  return JSON.stringify(e);
}
