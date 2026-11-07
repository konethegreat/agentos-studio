# Capability parity

A capability scorecard, not a feature-copy list. The point is to know where AgentOS Studio stands
against the tools a user might compare it to, and to be explicit about where we choose a different
approach.

Status legend: **yes** shipped, **part** partial, **plan** on the roadmap, **no** out of scope,
**differ** intentionally different.

## Chat and agent work

| Capability | Studio | Notes | Plan |
|---|---|---|---|
| Streaming chat with tool cards | yes | MCP cards show server, tool, input, result, duration | |
| Session history from disk | yes | Hermes `state.db`, search, rename, delete, export | |
| Session resume | yes | `hermes --resume`, and continuation from the terminal | |
| Fork a chat at any message | yes | new Hermes session via `hermes sessions import`, linked as a branch | text and tool-call markers only; tool outputs are not copied |
| Working-directory scoping per chat | yes | folder picker plus workspace tree | |
| Real terminal in the app | yes | PTY tabs: shell, Hermes, Hermes TUI, Claude Code | |
| Hand a chat to the terminal | yes | "Continue in terminal" resumes the session | |
| Slash commands | yes | plus a command palette | |
| Plan or step mode | yes | Plan first runs the turn as Hermes `/plan` (read-only, scoped to the folder); the plan shows in the chat and runs only after Approve | |
| Checkpoints and rollback of file edits | yes | new Hermes chats use `--checkpoints`; Undo last turn opens the Hermes TUI on the chat with `/rollback` typed (the gateway refuses it for single-query turns) | |
| Approve dangerous commands from the chat | no | single-query turns deny them (`approvals.single_query_mode`); ACP can ask, but is not the transport yet ([ACP spike](ACP-SPIKE.md)) | after Hermes adds creation policy to ACP |
| Stop a running turn | part | the run process is ended; ACP `session/cancel` works in the spike | with ACP |
| Missions that run with the app closed | yes | Hermes missions run on `hermes kanban`; the dispatcher lives in the Hermes gateway | |
| Background and scheduled tasks | yes | Hermes cron jobs listed from `cron/jobs.json`; create, pause, resume, run and remove through `hermes cron` (MCP and Providers > Hermes) | |
| Hermes built-in memory and journey | yes | MEMORY.md, USER.md, `hermes memory status` and `hermes journey --json` in Memory > Hermes; edit and delete only through `hermes journey` | |
| Hermes usage report | yes | `hermes insights` on demand (MCP and Providers > Hermes) | |
| Supply-chain audit | yes | `hermes security audit --json` in Safety | |
| Mission notices to a phone | yes | `hermes send` to a target you set, when a mission is done or fails | |
| Parallel agents on one repo | plan | Hermes kanban worktree workspaces; `chat -q` refuses `--worktree` | next |

## Models

| Capability | Studio | Notes | Plan |
|---|---|---|---|
| Claude subscription without an API key | yes | DirectSDK plugin, signed in through Claude Code | |
| Multiple providers | yes | every provider in Hermes' catalog, switched in Providers | |
| Every provider Hermes supports | yes | read from `provider_models_cache.json`; no app release needed | |
| Live model catalog | yes | Hermes' model cache, refreshed by Hermes | |
| New model badges and deprecation warnings | plan | diff the Hermes model cache between runs | later |
| Model aliases | plan | no Hermes command manages them yet | later |
| Fallback chains | plan | `fallback_providers`, written with `hermes config set` | next |
| Credential pools | plan | `hermes auth` list, priority, reset, remove | next |
| Multi-model mixtures | plan | `hermes moa` presets | next |
| Per-workspace model defaults | plan | a folder pins a model for new chats | next |
| Automatic model selection | part | shadow router scores real missions and explains its pick; changes nothing | after more mission data |
| Cost guardrails and budgets | yes | per-mission budgets from Hermes usage data, metered providers only; time caps | |
| Per-model scorecards from real sessions | part | approval rate and cost per role, provider and model from missions | |

## Memory and knowledge

| Capability | Studio | Notes | Plan |
|---|---|---|---|
| Local markdown memory as source of truth | yes | Obsidian vault, wikilinks, frontmatter | |
| Shared memory tools for agents | yes | `agentos-hub` read and write, search with ranking | |
| Cloud dashboard | part | Notion publishing: digest, tasks, missions, fleet | |
| Two-way Notion sync with a ledger | part | publish with `sync_ledger` and hash skip; only the inbox is pulled back | later |
| Show what was read and written per turn | yes | memory ledger per turn, including Hermes' own memory tool | |
| Project rules inherited by agents | yes | constitution, `CLAUDE.md`, `AGENTS.md`, `GEMINI.md` | |

## Engineering and delivery

| Capability | Studio | Notes | Plan |
|---|---|---|---|
| GitHub operations in the UI | yes | auth, repos, branches, commits, PRs, issues, Actions | |
| GitHub operations from chat | yes | `/gh` and "Ask Hermes" hand-off | |
| MCP server management across agents | yes | registry, native probe, catalog | |
| Computer use | part | Hermes toolset and cua-driver; no dedicated view | later |
| Per-tool permissions | yes | per MCP tool, enforced inside the agentos-hub server, plus per action in Studio | |
| Audit log | yes | every guarded action, terminal open, policy change | |
| Self-update with backup and rollback | yes | Studio, Hermes and Claude Code | |
| Signed builds and in-place updates | yes | Tauri updater with a minisign-signed manifest; no code signing certificate (Windows shows unknown publisher) | |

## Transparency

| Capability | Studio | Notes | Plan |
|---|---|---|---|
| Live tool timeline | yes | cards plus worker and PTY output | |
| Reasoning panel | no | Hermes streams no reasoning to `chat -q` or ACP; it shows live in the terminal | when Hermes streams it |
| Token and cost per turn | part | usage strip: context, session spend, compactions, credits | |
| Pause, stop, intervene | part | stop ends the run; ACP cancel is proven but not wired | with ACP |
| Router decision explanation | part | the shadow router prints its reason | |

## Where we deliberately differ

- **One binary, two roles.** The desktop app is also the MCP server that every agent connects to.
  No separate daemon to install, and the same code path serves the UI and the agents.
- **The vault is the contract.** Tasks, memory and exported sessions are plain Markdown with
  frontmatter, readable and editable by a human without the app. Other tools keep state in a
  proprietary store and export as an afterthought.
- **Routing is deterministic and auditable.** Auto model selection will not spend a model call to
  pick a model, and will not change your default silently. Every decision has a written reason and
  a pin override.
- **Two agents, one workspace.** Hermes and Claude Code share the same chat UI, the same workspaces
  and the same tools, instead of forcing a choice of app per agent.
- **Bring your own everything.** Providers, MCP servers, toolsets and permissions are all visible
  and editable, driven by the CLIs you already use, with no hidden configuration layer.
- **Local first.** Sessions, workspaces, outcomes and routing history are local SQLite and
  Markdown. Nothing is uploaded unless exported.

## How this list is maintained

Reviewed at the start of each phase, and updated when a capability ships or a competitor changes
something that matters. A row moves to "yes" only with a verification note in the release record,
not because code exists.

## Memory tools (v0.4.1)

| Hermes CLI | Studio |
|---|---|
| Reading every note before a task | `project_brief(repo)` plus `recall(query)` at task start, with `01_Projects`, `03_Memory` and `02_Agent_Bus/Completed` indexed in FTS5 |
| Ad hoc grep | `recall` with bm25, recency decay and wikilink in-degree, superseded notes last |
| Writing notes by hand | `remember(type, topic, content)` with decision, fact, convention, episode and gotcha |
| Decision history | only `type: decision` reaches `DECISION_LOG.md` |
