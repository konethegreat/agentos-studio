// Typed bindings for the v0.3 Rust commands: terminals, updates, organisation, GitHub, safety.
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { SessionSummary } from "./api";

// ---------------------------------------------------------------- terminals

export type PtyProfile = "shell" | "hermes" | "hermes-tui" | "hermes-resume" | "hermes-tui-resume" | "claude" | "claude-resume";

export interface PtyInfo {
  id: string;
  profile: PtyProfile;
  title: string;
  cwd: string;
  pid: number | null;
  started_at: number;
}

export interface PtySpawn {
  profile: PtyProfile;
  session_id?: string | null;
  cwd?: string | null;
  cols?: number;
  rows?: number;
  /** Typed (not sent) once the program has drawn and gone quiet. */
  type_when_ready?: string | null;
}

// ---------------------------------------------------------------- updates

export interface ChangeGroup {
  title: string;
  items: string[];
}

export interface Changelog {
  versions: string[];
  highlights: string[];
  groups: ChangeGroup[];
  url: string;
}

export type Product = "hermes" | "claude-code" | "studio";

export interface ProductUpdate {
  product: Product;
  label: string;
  installed: boolean;
  current: string | null;
  latest: string | null;
  state: "up_to_date" | "available" | "unknown" | "not_installed";
  detail: string;
  changelog: Changelog | null;
  skipped: boolean;
  snoozed_until: number | null;
  alert: boolean;
  can_update: boolean;
  update_note: string;
  checked_at: number;
}

export interface UpdateReport {
  products: ProductUpdate[];
  checked_at: number;
}

export interface UpdateProgress {
  job: string;
  product: Product;
  level: "cmd" | "out" | "err" | "step" | "ok" | "error";
  line: string;
  done: boolean;
  ok?: boolean;
}

export interface BackupManifest {
  id: string;
  product: Product;
  created_at: number;
  from_version: string;
  to_version: string;
  hermes_commit: string | null;
  hermes_install_dir: string | null;
  hermes_snapshot: string | null;
  files: string[];
  note: string;
  dir: string;
}

// ---------------------------------------------------------------- organisation

export interface Workspace {
  id: string;
  name: string;
  color: string | null;
  sort: number;
  archived: boolean;
  created_at: number;
}

export interface Folder {
  id: string;
  workspace_id: string;
  parent_id: string | null;
  name: string;
  path: string | null;
  project_slug: string | null;
  pinned: boolean;
  archived: boolean;
  sort: number;
  created_at: number;
  tags: string[];
}

export interface Attachment {
  id: string;
  owner_kind: "chat" | "folder";
  owner_id: string;
  path: string;
  label: string | null;
  added_at: number;
  exists: boolean;
}

export interface HermesProject {
  id: string;
  slug: string;
  name: string;
  primary_path: string | null;
  archived: boolean;
  folders: string[];
}

export interface PlacedChat extends SessionSummary {
  folder_id: string | null;
  workspace_id: string | null;
  auto: boolean;
  studio_pinned: boolean;
  archived: boolean;
  tags: string[];
}

export interface OrgSnapshot {
  workspaces: Workspace[];
  folders: Folder[];
  attachments: Attachment[];
  tags: string[];
  projects: HermesProject[];
  chats: PlacedChat[];
}

export interface FolderPatch {
  name?: string;
  path?: string;
  pinned?: boolean;
  archived?: boolean;
  workspace_id?: string;
  parent_id?: string;
  sort?: number;
}

// ---------------------------------------------------------------- GitHub

export interface GhAuth {
  installed: boolean;
  logged_in: boolean;
  login?: string;
  scopes?: string;
  host?: string;
}

export interface GhRepo {
  nameWithOwner: string;
  description: string | null;
  isPrivate: boolean;
  isArchived: boolean;
  updatedAt: string;
  url: string;
  defaultBranchRef: { name: string } | null;
  stargazerCount: number;
  primaryLanguage: { name: string } | null;
}

export interface GhUser {
  login: string;
}

export interface GhCheck {
  name?: string;
  context?: string;
  status?: string;
  conclusion?: string | null;
  state?: string;
  detailsUrl?: string;
  targetUrl?: string;
}

export interface GhPr {
  number: number;
  title: string;
  author: GhUser;
  headRefName: string;
  baseRefName: string;
  isDraft: boolean;
  reviewDecision: string;
  updatedAt: string;
  url: string;
  additions: number;
  deletions: number;
  state: string;
  labels: { name: string }[];
  statusCheckRollup: GhCheck[] | null;
}

export interface GhComment {
  author: GhUser;
  body: string;
  createdAt: string;
}

export interface GhPrDetail extends GhPr {
  body: string;
  mergeable: string;
  mergeStateStatus: string;
  reviews: { author: GhUser; state: string; body: string; submittedAt: string }[];
  comments: GhComment[];
  files: { path: string; additions: number; deletions: number }[];
  commits: { oid: string; messageHeadline: string }[];
  createdAt: string;
}

export interface GhIssue {
  number: number;
  title: string;
  author: GhUser;
  labels: { name: string }[];
  state: string;
  updatedAt: string;
  url: string;
  comments: GhComment[] | { totalCount: number };
  assignees: GhUser[];
}

export interface GhIssueDetail extends GhIssue {
  body: string;
  comments: GhComment[];
  createdAt: string;
}

export interface GhRun {
  databaseId: number;
  number: number;
  displayTitle: string;
  status: string;
  conclusion: string;
  workflowName: string;
  headBranch: string;
  event: string;
  createdAt: string;
  updatedAt: string;
  url: string;
}

export interface GhRunDetail extends GhRun {
  jobs: { name: string; status: string; conclusion: string; steps: { name: string; status: string; conclusion: string; number: number }[] }[];
}

export interface GhBranch {
  name: string;
  protected: boolean;
  commit: { sha: string };
}

export interface GhCommit {
  sha: string;
  html_url: string;
  commit: { message: string; author: { name: string; date: string } };
  author: { login: string } | null;
}

// ---------------------------------------------------------------- safety

export interface PolicyView {
  id: string;
  group: string;
  label: string;
  risk: "low" | "medium" | "high";
  default_policy: Policy;
  reversible: boolean;
  policy: Policy;
}

export type Policy = "allow" | "ask" | "deny";

export interface AuditEntry {
  id: number;
  at: number;
  action: string;
  target: string;
  detail: string;
  outcome: string;
}

// ---------------------------------------------------------------- bindings

type Approved = { approved?: boolean };

// ---------------------------------------------------------------- Hermes surfaces

export interface JourneyNode {
  id: string;
  label: string;
  kind: string;
  timestamp?: number;
  category?: string;
  useCount?: number;
  state?: string;
  createdBy?: string;
  memorySource?: string;
}

export interface HermesMemory {
  home: string;
  memory_path: string;
  memory: string;
  user_path: string;
  user: string;
  status: string;
  journey: { nodes?: JourneyNode[]; stats?: Record<string, unknown> } | null;
}

export interface CronJob {
  id: string;
  name: string | null;
  prompt: string | null;
  schedule_display: string | null;
  state: string | null;
  enabled: boolean;
  next_run_at: string | null;
  last_run_at: string | null;
  last_status: string | null;
  last_error: string | null;
  workdir: string | null;
  deliver: string | null;
}

export interface CronSpec {
  schedule: string;
  prompt: string;
  name?: string;
  workdir?: string;
  paused?: boolean;
}

export interface HermesProfile {
  name: string;
  path: string;
  model: string;
  provider: string;
  roles: string[];
}

export interface AuditFinding {
  package: string;
  version: string;
  ecosystem: string;
  source: string;
  vuln_id: string;
  severity: string;
  summary: string;
  fixed_versions: string[];
}

export interface Route {
  provider: string;
  model: string;
  base_url?: string;
}

export interface Credential {
  provider: string;
  index: number;
  label: string;
  kind: string;
  id: string;
  priority: number;
  source: string;
  current: boolean;
  status: string;
}

export interface Moa {
  default_preset: string;
  active_preset: string;
  presets: { name: string; enabled: boolean; reference_models: Route[]; aggregator: Route | null }[];
}

export interface FolderModel {
  folder: string;
  provider: string;
  model: string;
}

export interface PermissionOption {
  id: string;
  name: string;
  kind: string;
}

export const studio = {
  chat: {
    answerPermission: (requestId: string, optionId: string | null) => invoke<void>("answer_chat_permission", { requestId, optionId }),
    transport: () => invoke<"acp" | "cli">("hermes_chat_transport"),
    setTransport: (acp: boolean) => invoke<void>("set_hermes_chat_transport", { acp }),
    warm: () => invoke<void>("warm_hermes_chat"),
  },
  models: {
    fallbackChain: () => invoke<Route[]>("hermes_fallback_chain"),
    setFallbackChain: (chain: Route[], a: Approved = {}) => invoke<void>("hermes_set_fallback_chain", { chain, ...a }),
    credentials: () => invoke<Credential[]>("hermes_credentials"),
    credentialAction: (provider: string, id: string, action: "priority" | "reset" | "remove", priority: number | null, a: Approved = {}) =>
      invoke<string>("hermes_credential_action", { provider, id, action, priority, ...a }),
    moa: () => invoke<Moa>("hermes_moa"),
    setActiveMoa: (preset: string) => invoke<void>("hermes_set_active_moa", { preset }),
    folderModels: () => invoke<FolderModel[]>("folder_models"),
    changes: () =>
      invoke<{ new: { provider: string; model: string; first_seen: number }[]; gone: { provider: string; model: string; used_by: string[] }[] }>("model_changes"),
    setFolderModels: (entries: FolderModel[]) => invoke<void>("set_folder_models", { entries }),
  },
  plans: {
    prompt: (task: string, cwd: string) => invoke<string>("plan_prompt", { task, cwd }),
    runPrompt: (path: string) => invoke<string>("plan_run_prompt", { path }),
    latest: (cwd: string, since: number) => invoke<{ path: string; body: string; modified: number } | null>("latest_plan", { cwd, since }),
  },
  hermes: {
    memory: () => invoke<HermesMemory>("hermes_memory_view"),
    journeyDelete: (node: string, a: Approved = {}) => invoke<string>("hermes_journey_delete", { node, ...a }),
    insights: (days: number) => invoke<string>("hermes_insights", { days }),
    securityAudit: () => invoke<{ total_components_scanned?: number; finding_count?: number; findings?: AuditFinding[] }>("hermes_security_audit"),
    cronJobs: () => invoke<CronJob[]>("hermes_cron_jobs"),
    cronAction: (id: string, action: "pause" | "resume" | "run" | "remove", a: Approved = {}) => invoke<string>("hermes_cron_action", { id, action, ...a }),
    cronCreate: (spec: CronSpec, a: Approved = {}) => invoke<string>("hermes_cron_create", { spec, ...a }),
    profiles: () => invoke<HermesProfile[]>("hermes_profiles"),
    setRoleProfile: (role: "implementer" | "reviewer", profile: string) => invoke<void>("hermes_set_role_profile", { role, profile }),
    notifyTarget: () => invoke<string | null>("hermes_notify_target"),
    setNotifyTarget: (target: string, test: boolean) => invoke<string>("hermes_set_notify_target", { target, test }),
    timeCaps: () => invoke<{ mission_max_runtime: number | null; run_budget_seconds: number | null }>("hermes_time_caps"),
    setTimeCaps: (missionMaxRuntime: number | null, runBudgetSeconds: number | null) => invoke<void>("hermes_set_time_caps", { missionMaxRuntime, runBudgetSeconds }),
    parallelPerRepo: () => invoke<number>("missions_parallel_per_repo"),
    setParallelPerRepo: (n: number) => invoke<void>("set_missions_parallel_per_repo", { n }),
  },
  pty: {
    spawn: (request: PtySpawn) => invoke<PtyInfo>("pty_spawn", { request }),
    write: (id: string, data: string) => invoke<void>("pty_write", { id, data }),
    resize: (id: string, cols: number, rows: number) => invoke<void>("pty_resize", { id, cols, rows }),
    kill: (id: string) => invoke<void>("pty_kill", { id }),
    list: () => invoke<PtyInfo[]>("pty_list"),
    snapshot: (id: string) => invoke<{ data: string; seq: number }>("pty_snapshot", { id }),
  },
  updates: {
    check: () => invoke<UpdateReport>("check_updates"),
    last: () => invoke<ProductUpdate[] | null>("last_update_report"),
    apply: (product: Product, a: Approved = {}) => invoke<string>("apply_update", { product, ...a }),
    skip: (product: Product, version: string) => invoke<void>("skip_update", { product, version }),
    snooze: (product: Product, hours: number) => invoke<number>("snooze_update", { product, hours }),
    clear: (product: Product) => invoke<void>("clear_update_choice", { product }),
    backups: () => invoke<BackupManifest[]>("list_update_backups"),
    rollback: (backupId: string, a: Approved = {}) => invoke<string>("rollback_update", { backupId, ...a }),
  },
  org: {
    snapshot: (query: string | null) => invoke<OrgSnapshot>("org_snapshot", { query }),
    createWorkspace: (name: string) => invoke<Workspace>("org_create_workspace", { name }),
    updateWorkspace: (id: string, p: { name?: string; archived?: boolean; color?: string }) => invoke<void>("org_update_workspace", { id, ...p }),
    deleteWorkspace: (id: string, a: Approved = {}) => invoke<void>("org_delete_workspace", { id, ...a }),
    createFolder: (workspaceId: string, parentId: string | null, name: string, path: string | null) =>
      invoke<string>("org_create_folder", { workspaceId, parentId, name, path }),
    updateFolder: (id: string, patch: FolderPatch) => invoke<void>("org_update_folder", { id, patch }),
    deleteFolder: (id: string, a: Approved = {}) => invoke<void>("org_delete_folder", { id, ...a }),
    placeChat: (sessionId: string, folderId: string | null, workspaceId: string | null) =>
      invoke<void>("org_place_chat", { sessionId, folderId, workspaceId }),
    setChatFlags: (sessionId: string, p: { pinned?: boolean; archived?: boolean }) => invoke<void>("org_set_chat_flags", { sessionId, ...p }),
    setTags: (kind: "chat" | "folder", itemId: string, tags: string[]) => invoke<string[]>("org_set_tags", { kind, itemId, tags }),
    addAttachment: (kind: "chat" | "folder", ownerId: string, path: string, label: string | null = null) =>
      invoke<string>("org_add_attachment", { kind, ownerId, path, label }),
    removeAttachment: (id: string) => invoke<void>("org_remove_attachment", { id }),
    contextFiles: (sessionId: string | null, folderId: string | null) => invoke<string[]>("org_context_files", { sessionId, folderId }),
    linkProject: (folderId: string, a: Approved = {}) => invoke<string>("org_link_project", { folderId, ...a }),
  },
  gh: {
    auth: () => invoke<GhAuth>("gh_auth_status"),
    login: () => invoke<string>("gh_login"),
    repos: (limit = 100) => invoke<GhRepo[]>("gh_repos", { limit }),
    repoForDir: (dir: string) => invoke<{ nameWithOwner: string; url: string; defaultBranchRef: { name: string } | null }>("gh_repo_for_dir", { dir }),
    branches: (repo: string) => invoke<GhBranch[]>("gh_branches", { repo }),
    commits: (repo: string, branch: string | null) => invoke<GhCommit[]>("gh_commits", { repo, branch }),
    prs: (repo: string, state: string) => invoke<GhPr[]>("gh_prs", { repo, state }),
    pr: (repo: string, number: number) => invoke<GhPrDetail>("gh_pr_detail", { repo, number }),
    prDiff: (repo: string, number: number) => invoke<string>("gh_pr_diff", { repo, number }),
    merge: (repo: string, number: number, method: string, deleteBranch: boolean, a: Approved = {}) =>
      invoke<string>("gh_pr_merge", { repo, number, method, deleteBranch, ...a }),
    review: (repo: string, number: number, event: string, body: string, a: Approved = {}) =>
      invoke<string>("gh_pr_review", { repo, number, event, body, ...a }),
    createPr: (dir: string, title: string, body: string, base: string | null, head: string | null, draft: boolean, a: Approved = {}) =>
      invoke<string>("gh_pr_create", { dir, title, body, base, head, draft, ...a }),
    issues: (repo: string, state: string) => invoke<GhIssue[]>("gh_issues", { repo, state }),
    issue: (repo: string, number: number) => invoke<GhIssueDetail>("gh_issue_detail", { repo, number }),
    createIssue: (repo: string, title: string, body: string, a: Approved = {}) => invoke<string>("gh_issue_create", { repo, title, body, ...a }),
    comment: (repo: string, kind: "issue" | "pr", number: number, body: string, a: Approved = {}) =>
      invoke<string>("gh_comment", { repo, kind, number, body, ...a }),
    closeIssue: (repo: string, number: number, reason: string, a: Approved = {}) => invoke<string>("gh_issue_close", { repo, number, reason, ...a }),
    runs: (repo: string, branch: string | null) => invoke<GhRun[]>("gh_runs", { repo, branch }),
    run: (repo: string, id: number) => invoke<GhRunDetail>("gh_run_detail", { repo, id }),
    runLog: (repo: string, id: number) => invoke<string>("gh_run_failed_log", { repo, id }),
    rerun: (repo: string, id: number, failedOnly: boolean, a: Approved = {}) => invoke<string>("gh_run_rerun", { repo, id, failedOnly, ...a }),
  },
  os: {
    openExternal: (url: string) => invoke<void>("open_external", { url }),
    reveal: (path: string) => invoke<void>("reveal_path", { path }),
  },
  safety: {
    policies: () => invoke<PolicyView[]>("list_policies"),
    setPolicy: (action: string, policy: Policy) => invoke<void>("set_policy", { action, policy }),
    audit: (limit = 300, query: string | null = null) => invoke<AuditEntry[]>("list_audit", { limit, query }),
  },
};

export const studioEvents = {
  ptyData: (cb: (e: { id: string; data: string; seq: number }) => void): Promise<UnlistenFn> =>
    listen<{ id: string; data: string; seq: number }>("agentos://pty-data", (e) => cb(e.payload)),
  ptyExit: (cb: (e: { id: string; code: number | null }) => void): Promise<UnlistenFn> =>
    listen<{ id: string; code: number | null }>("agentos://pty-exit", (e) => cb(e.payload)),
  updates: (cb: (r: UpdateReport) => void): Promise<UnlistenFn> => listen<UpdateReport>("agentos://updates", (e) => cb(e.payload)),
  updateProgress: (cb: (e: UpdateProgress) => void): Promise<UnlistenFn> =>
    listen<UpdateProgress>("agentos://update-progress", (e) => cb(e.payload)),
};

/** Parsed form of the Rust `APPROVAL_REQUIRED|<action>|<summary>` error. */
export function approvalRequest(e: unknown): { action: string; summary: string } | null {
  const s = typeof e === "string" ? e : e instanceof Error ? e.message : "";
  if (!s.startsWith("APPROVAL_REQUIRED|")) return null;
  const [, action, ...rest] = s.split("|");
  return { action, summary: rest.join("|") };
}
