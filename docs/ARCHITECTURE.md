# AgentOS Studio architecture (v0.3)

AgentOS Studio is a GUI control plane over the Hermes agent runtime. It owns no agent logic. Everything it does goes through one of three Hermes surfaces:

1. The public CLI (`hermes chat --format stream-json`, `hermes update`, `hermes project`, `hermes mcp`, `hermes config set`, `hermes sessions delete`, `hermes import`).
2. Hermes's own files (`config.yaml`, `.env` key names only, `state.db`, `projects.db`, model caches), read directly and never written.
3. An interactive Hermes process inside a real pseudo terminal (ConPTY on Windows, openpty on macOS and Linux), for anything the CLI only exposes interactively (live reasoning, slash commands, approvals typed at the prompt).

Studio keeps a small store of its own (`studio.db`) only for things Hermes has no concept of: nested folders, chat placement, tags, attachments, permission policies, the audit log and update preferences.

## System diagram

```mermaid
flowchart LR
    subgraph UI["React 19 UI (WebView)"]
        Chat["Chat Studio<br/>workspace tree, stream, tool cards"]
        Term["Terminal dock<br/>xterm.js PTY tabs + command runner"]
        Upd["Update Center<br/>banner, changelog, backups"]
        GH["GitHub<br/>PRs, issues, Actions, branches, commits"]
        Safe["Safety<br/>policies, audit log"]
        Pal["Command palette (Ctrl+K)"]
        Appr["Approval modal"]
    end

    subgraph Core["Rust core (Tauri v2)"]
        CS["chat_stream"]
        PTY["pty (portable-pty)"]
        UPD["updates"]
        ORG["org (studio.db)"]
        GHM["github (gh CLI)"]
        PERM["permissions + audit"]
        HE["hermes_engine"]
        MCPM["mcp_manager"]
        VAULT["vault + bus"]
    end

    UI <-- "invoke / events" --> Core
    GHM --> PERM
    UPD --> PERM
    ORG --> PERM
    Appr -. "retry with approval" .-> PERM

    CS -- "stream-json turns" --> Hermes["hermes CLI"]
    PTY -- "interactive REPL / TUI" --> Hermes
    PTY --> Claude["claude CLI"]
    UPD -- "update --check, update --yes --backup, import" --> Hermes
    UPD -- "update, install <ver>" --> Claude
    UPD -- "releases API + installer" --> GHRel[("GitHub releases")]
    UPD -- "registry + CHANGELOG" --> NPM[("npm registry")]
    ORG -- "project create / add-folder" --> Hermes
    ORG -- "read" --> PDB[("projects.db")]
    HE -- "read" --> SDB[("state.db, config.yaml")]
    GHM -- "--json" --> GHCLI["gh CLI"]
    VAULT --> Obs[("Obsidian vault")]
    Hermes -- "Notion MCP" --> Notion[("Notion")]
    Core --> Store[("studio.db<br/>workspaces, folders, chat_meta,<br/>tags, attachments, permissions,<br/>audit, kv")]
```

## Startup (`boot.rs`, `single.rs`)

Every launch runs a fixed sequence before the app shows data, because the most common support
question is "it did not start" and the honest answer is usually one specific check.

| Step | What it does | If it fails |
|---|---|---|
| `dirs` | Creates the app's config, log and backup folders | Reported, boot continues |
| `path` | Repairs this process's PATH from the persisted user PATH plus known bin folders | Reported |
| `settings` | Loads `settings.json`, resolves the vault | Warning with the folder it used |
| `store` | Opens `studio.db` and applies migrations | Failed, boot continues read-only |
| `vault` | Checks the vault structure, creates missing folders only | Repaired or failed |
| `tools` | Probes git, node, gh, hermes and claude in parallel with a timeout each | Warning, names the missing tool |
| `hermes` | Parses `config.yaml`, reads provider, model and MCP counts | Failed with a pointer to the repair screen |
| `mcp` | Checks that `agentos-hub` is registered in Hermes and Claude Code | Warning with a pointer to sync |

Rules:

- **Only safe repairs run automatically.** Creating folders, applying migrations and fixing the
  PATH are idempotent and cannot destroy anything. Writing another tool's configuration,
  installing software and signing in are reported with a view to fix them, never done silently.
- **Every step is bounded.** Tool probes use a 4 second cap and run in parallel, so a missing tool
  costs one timeout, not five.
- **The report is streamed and cached.** Steps arrive on `agentos://boot` as they finish; the
  finished report is cached to `boot-snapshot.json` so the next launch paints instantly, and
  appended to `logs/boot.log` (last 400 runs).
- **Panics are logged.** `install_panic_hook` writes `logs/crash.log` with a message, location and
  backtrace, and the next launch shows the most recent entry on the boot screen. A GUI app with no
  console would otherwise die silently.
- **One instance only.** `single.rs` binds `127.0.0.1:47823`. A second launch asks the running
  instance to focus its window and exits, so a double click never produces a second heartbeat, a
  second vault watcher or a second fleet scan. No native dependency.

The UI defers its heavy loads (stack status, fleet scan, board) until the boot report says done, so
the first paint is never competing with a full probe.

## Components

| Module | Responsibility |
|---|---|
| `chat_stream.rs` | One streaming chat turn per message for Hermes or Claude Code; parses stream-json into UI events. |
| `acp_host.rs` | Hermes chat over ACP: creates sessions with interactive `hermes chat` (creation policy), runs turns on one long-lived `hermes acp` connection, routes updates and approval questions per session. `chat_stream.rs` drives it and falls back to `chat -q`. |
| `computer_use.rs` | Hermes computer use: toolset state from `hermes tools list`, the cua-driver Hermes installed (version, update, daemon), the `computer_use` results in `state.db` (read only), and screenshots served only from Hermes' image cache. |
| `notion_sync.rs` | Two-way Notion missions board: rows out per mission with a hash ledger; proposals and cancels in. |
| `model_layer.rs` | Hermes' model layer: fallback chain (`hermes config get/set fallback_providers`), credential pools (parsed `hermes auth list`; `auth.json` is never read), MoA presets, and Studio's per-folder default model for new chats. |
| `plans.rs` | Plan mode: the `/plan` prompt scoped to the chat folder, the newest plan in `.hermes/plans`, and the run prompt after approval. |
| `kanban.rs` | Hermes missions on Hermes' kanban board: create (model pinned at creation, implementer profile, `dir:` workspace, time cap), read runs, decide wait, review or block, collect worker session ids for spend, and run `hermes verify`. Kanban `done` is never Studio approval. |
| `learning.rs` (new) | The learning loop: one headless Hermes turn per finished mission writes validated lessons into `03_Memory` as typed notes, and repeated review blockers become one convention note per repository. Additive and idempotent; it never runs the reviewer. |
| `memory_quality.rs` (new) | The memory audit: finds notes whose backticked paths or functions no longer exist in the repository, asks one Hermes turn to judge pairs of active notes that share a topic and type, and marks the loser stale with `status: superseded` and `superseded_by`. Never deletes. Runs from the Brain panel and the `--memory-quality` headless flag, with an optional weekly cron job. |
| `hermes_surfaces.rs` | Hermes features shown, not rebuilt: built-in memory and journey, insights, security audit, cron jobs, profiles with Studio's role assignment, and `hermes send` for mission notices. Reads Hermes files directly; every change runs a Hermes command behind Studio's permission check. |
| `acp.rs` (spike, unwired) | Agent Client Protocol client for `hermes acp`: new or loaded session, one streamed turn, permission requests answered through a channel, cancel. Kept to re-run `tests/live_acp_spike.rs` against new Hermes releases; see `ACP-SPIKE.md`. |
| `pty.rs` (new) | Real terminals: spawn a shell, interactive Hermes, `hermes --resume <id>`, Claude Code or `claude --resume <id>` in a PTY; streams bytes as UTF-8 safe chunks, accepts keystrokes and resizes. |
| `updates.rs` (new) | Version detection for Hermes, Claude Code and Studio; changelog extraction; skip and snooze state; OS notifications; backup, update and rollback runners. |
| `org.rs` (new) | `studio.db`: workspaces, nested folders, chat placement, pins, tags, attachments, archive; links folders to Hermes projects through `hermes project`. |
| `github.rs` (new) | Typed wrappers over `gh ... --json` for auth, repos, branches, commits, PRs (diff, checks, review, merge, create), issues (create, comment, close) and Actions runs (logs, rerun). |
| `permissions.rs` (new) | Per-action policy (`allow`, `ask`, `deny`) enforced in Rust before any sensitive command runs, plus the append-only audit log. |
| `hermes_engine.rs` | Heartbeat, provider switchboard, model discovery, sessions in `state.db`. |
| `mcp_manager.rs` | MCP registry across four agents, native MCP client, catalog. |
| `cli_runner.rs` | One-shot command runner (kept for quick actions with captured output). |
| `vault.rs`, `bus.rs`, `mcp_stdio.rs` | Agent Bus, shared memory and the `agentos-hub` MCP server. `bus.rs` also starts headless workers (Hermes, Claude Code, Antigravity) for the scheduler and the event-driven watcher. |
| `worktree.rs` | One git worktree and branch per bus mission attempt, excluded locally, removed when the mission ends. |
| `stuck.rs` | Heartbeat per running mission (kanban times and session messages, worker process and log, worktree changes); one retry, then blocked. |
| `hub_identity.rs` | Per-agent hub tokens (hash only in `studio.db`), caller resolution, the write-as-yourself and read-only rules, `whoami`. |
| `agents.rs`, `agent_log.rs` | Agent registry (start mode, strengths, tools, cost model, quota headroom, health) and the schema 9 exchange log every cross-agent step writes to. |
| `handoff.rs`, `ask.rs` | The `handoff` packet builder (receiver starts in the same worktree) and `ask_agent` (one headless turn on another agent, loop guard in kv). |
| `council.rs`, `assign.rs` | Design-decision councils on different providers with an ADR pull request, and quota-aware assignment of `auto` missions. |
| `timeline.rs` | Read-only run timeline per mission from the exchange log, bus, worker logs, kanban, `state.db` and the memory ledger. |

### Approval flow

Sensitive commands take an `approved: bool` argument. `permissions::guard(action, approved)` runs first:

- `deny`: refused, audited as `denied`.
- `ask` and not approved: returns `APPROVAL_REQUIRED|<action>|<summary>`. The UI shows the approval modal (what will run, against what, whether it can be undone) and re-invokes with `approved = true`, optionally switching the policy to `allow`.
- `allow`, or `ask` and approved: runs, audited as `ok` or `error` with the error text.

The check lives in Rust, so a UI bug cannot skip it.

### Who owns what with Hermes

Hermes owns execution of Hermes work: sessions, tools, approvals of dangerous commands, checkpoints
and its own memory. Studio owns cross-agent policy: orchestration across Hermes, Claude Code and
Antigravity, the mission review gate and merge, the vault, and visibility. Studio talks to Hermes
through its CLI (`hermes chat -q`, `hermes sessions`, `hermes config set`, ...). The Agent Client
Protocol was evaluated as the chat transport and deferred (`ACP-SPIKE.md`): Hermes approvals in
Studio chat are therefore still denied by `approvals.single_query_mode`.

## Data model (`studio.db`)

Location: `<config dir>/agentos-studio/studio.db` (`%APPDATA%\agentos-studio` on Windows). Hermes sessions stay in Hermes's `state.db`; Studio stores placement only.

```sql
workspaces  (id TEXT PK, name, color, sort, archived, created_at)
folders     (id TEXT PK, workspace_id -> workspaces ON DELETE CASCADE,
             parent_id -> folders ON DELETE CASCADE, name, path, project_slug,
             pinned, archived, sort, created_at)
chat_meta   (session_id TEXT PK, folder_id -> folders ON DELETE SET NULL,
             workspace_id -> workspaces ON DELETE SET NULL, pinned, archived, updated_at)
tags        (name TEXT PK, color)
item_tags   (item_kind 'chat'|'folder', item_id, tag -> tags ON DELETE CASCADE, PK all three)
attachments (id TEXT PK, owner_kind 'chat'|'folder', owner_id, path, label, added_at)
permissions (action TEXT PK, policy 'allow'|'ask'|'deny')
audit       (id INTEGER PK, at, action, target, detail, outcome)
kv          (key TEXT PK, value)          -- update skips, snoozes, notified versions
```

Placement rules:

- A chat with a `chat_meta.folder_id` lives in that folder.
- Otherwise it is placed automatically in the folder whose `path` is the longest prefix of the session `cwd` (the same rule Hermes projects use). Auto-placed chats are marked so the user can see why they are there.
- Everything else is "Unfiled".
- Deleting a folder never deletes chats: they go back to Unfiled. Deleting a Hermes session is a separate, guarded action.

Attachments are references to files on disk, not copies. When a chat with attachments sends a message, the attached paths (chat plus its folder chain) are listed at the top of the prompt so the agent reads them with its own file tools, and the chat shows exactly which files were included.

## Folder structure

```
src/
  App.tsx                    shell, navigation, palette, approval and update providers
  components/
    ApprovalProvider.tsx     guarded calls and the approval modal
    CommandPalette.tsx       Ctrl+K
    PtyTerminal.tsx          xterm.js terminal bound to a Rust PTY
    CliDrawer.tsx            terminal dock: command runner tab + PTY tabs
    UpdateBanner.tsx         in-app update alert
    WorkspaceTree.tsx        workspaces > folders > chats, drag and drop
  views/
    ChatStudio.tsx           chat (now with the workspace tree and terminal handoff)
    UpdateCenter.tsx         versions, changelogs, backups, rollback
    GitHubView.tsx           repos, PRs, issues, Actions, branches, commits
    SafetyCenter.tsx         permission policies and audit log
  lib/
    api.ts                   typed bindings for every Rust command and event
    org.ts                   tree building and placement rules (pure, tested by tsc)
src-tauri/src/
  pty.rs  updates.rs  org.rs  github.rs  permissions.rs  (new in v0.3)
docs/
  ARCHITECTURE.md  ROADMAP.md  adr/
```

## Navigation shell (v0.4.1)

The twelve-item sidebar is gone. Navigation is a 56 px icon rail that expands to 208 px on hover or
when pinned, with the pin stored in `studio.db` (`kv.nav.pinned`). Five groups:

| Group | Tabs |
|---|---|
| Chat | Chat Studio |
| Work | Projects, Mission Control, Fleet |
| Brain | Memory, Skills, Constitution |
| Code | GitHub |
| System | MCP and Providers, Updates, Safety, Setup, Diagnostics |

Badges are counts, not decoration: open bus tasks on Work, pending updates on System, failing
workflow runs on Code (from `gh run list` for the repository bound to the current folder, refreshed
every three minutes). `Alt+1..5` select a group, `Ctrl+K` still reaches every tab by name.

The top bar no longer repeats provider and model, which belong to the per-chat switcher. It carries
the Hermes version and a single health chip that opens Diagnostics.

## Working directory resolution

A GUI inherits whatever directory its launcher had, and on Windows that is sometimes
`C:\Windows\System32`. `workdir.rs` resolves a new chat's folder in this order: last folder used
(`studio.db` kv `chat.cwd`), the active project root (`projects.db`, falling back to the only
project when there is exactly one), then the user's home. A candidate is rejected when it is
relative, missing, under `%WINDIR%`, or any path segment is `system32`. `pick()` is pure so the rule
is unit tested without touching the environment.

## Memory ledger

`memory_ledger` in `studio.db` (schema 3) records `(session_id, turn, op, target, path, tool, at)`
for every memory-touching tool call in a chat turn. It is written from the `tool_use` events the
chat stream already parses, so the strip under a message is derived from evidence, not from the
model's summary. Classification is by tool name: `recall`, `search_shared_memory`, `project_brief`
and read tools are reads; `remember`, `write_shared_memory` and edit tools are writes; any Notion
tool is `target: notion`; agentos-hub memory tools are `target: vault`; everything else that touches
a file is `target: file`. Turn numbers come from the UI, which sends the 1-based user turn with each
message, so rows attach to the message that caused them.

## Typed memory

`03_Memory` notes gain `type` (`decision`, `fact`, `convention`, `episode`, `gotcha`), `status`
(`active`, `superseded`), and optional `supersedes`, `source_session`, `agent` and `repo`.
`DECISION_LOG.md` is appended to only by `type: decision`, so the log keeps meaning what it says.
Notes written before types existed have no `type` and read as `fact` at read time; nothing is
rewritten. Superseding is the one deliberate write to an older note: it flips that note to
`status: superseded` and records `superseded_by`.

## Brain view

The Brain view shows the two memory stores side by side when there is room, and switches to Vault
and Hermes tabs below 1280 px: the window's minimum width is 1100 px, so the split would otherwise
have to work in two 550 px columns. The Vault column is the ranked index over `03_Memory` (type,
links, modified time) plus the recent ledger activity, which is what every agent reads through
`recall` and writes through `remember`. The Hermes column reads `MEMORY.md` and `USER.md` from the
Hermes home (`-p <profile>` when a profile is selected), marks them read-only, and lists the journey
nodes Hermes builds from skills and memory cards. Both columns report their own failure: a missing
file, an unreadable status or a journey
parse error shows in that column instead of blanking the screen. Deleting a journey node is the only
write the view offers, and it goes through `hermes.journey.delete` with the default policy `ask`.
Hermes does not expose a non-interactive edit: `hermes journey edit` opens `$EDITOR`, so the view
does not offer one. The Vault column also carries the Quality panel, which shows the last audit,
the stale and superseded counts, each change, and whether the weekly job is scheduled; the
Learning panel shows lessons written from finished missions.

## Learning loop

`learning.rs` turns finished work into memory. When a mission is `done` or `failed`, one short
headless Hermes turn receives the goal, acceptance criteria, review verdict and note, the files the
pull request changed and its summary, and must answer with a JSON array of one to five lessons
(`decision`, `convention` or `gotcha`). Every lesson is written through `Vault::write_typed_memory`
with `agent: agentos-learning`, the mission's `repo` and the turn's `source_session`. The answer is
all or nothing: a single malformed entry drops the whole answer and nothing is written.
`learning.done.<mission id>` records a completed extraction, so a rerun skips the turn. A dropped
answer is not recorded, so the next run may try again.

The second path is deterministic. Rejected missions from the last 30 days are grouped by their
normalised review reasons (bullet lists first, otherwise the first sentence). A group of two or more
writes one convention note named `Review blockers in <repo>`, links the missions and records their
ids in the store so a rerun writes nothing. `conventions_for` condenses those notes newest first and
caps them at 1500 characters for a future implementer prompt.

The model turn is injected (`lessons_with`), so the write path, the skip rule and the retry after a
dropped answer are unit tested without running Hermes. The live check (`tests/live_learning.rs`)
is ignored by default and runs only under the live-machine lock when the owner has allowed Hermes
use.

## Memory quality

`memory_quality.rs` audits the vault without deleting anything. A note is stale when it names a
backticked path (`src/`, `src-tauri/` or `docs/`, with an optional `:line`) that does not exist in
the repository checkout, or a backticked function (`name()` or `module::name`) that
`git grep -w` cannot find. A folder that is not a git checkout returns "unknown" and the note is
left alone. Contradiction candidates are two active notes of the same normalised topic and type
(`convention` or `fact`) whose bodies differ and carry a cheap opposition signal (one says "never"
where the other says "always", or the numbers disagree). One short Hermes turn answers a single
strict JSON object per pair, `{"verdict":"contradiction"|"compatible","keep":<note>,"reason":...}`;
a malformed answer drops every verdict and nothing is written.

A confirmed stale note or contradiction loser is marked through the existing supersede path:
frontmatter gains `status: superseded`, `superseded_by` and `superseded_reason`, and `updated`
moves. A note edited in the last seven days is reported only and never changed. The report is
stored in `studio.db` under `memory_quality.last_run` on a real run, so the panel and the weekly
job agree. The turn is injected (`run_with`), so every write path is unit tested offline.

The audit runs on demand from the Brain Quality panel and headlessly:
`agentos-studio.exe --memory-quality --vault <vault>` (add `--dry-run` to print changes without
writing). "Schedule weekly check" writes `agentos-memory-quality.py` under the Hermes scripts
folder and creates `0 6 * * 1 --name agentos-memory-quality --no-agent --script <path>` behind the
`hermes.cron.change` permission. The live check (`tests/live_memory_quality.rs`) is ignored by
default.
