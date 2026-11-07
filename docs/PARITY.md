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
| Plan or step mode | plan | approve the plan before tools run | v0.6.0 |
| Checkpoints and rollback of file edits | part | Hermes `--checkpoints` exists; `/rollback` is refused on the gateway, served only in the Ink TUI | v0.10 |
| Approve dangerous commands from the chat | no | single-query turns deny them (`approvals.single_query_mode`); ACP can ask, but is not the transport yet ([ACP spike](ACP-SPIKE.md)) | after Hermes adds creation policy to ACP |
| Stop a running turn | part | the run process is ended; ACP `session/cancel` works in the spike | with ACP |
| Background and scheduled tasks | part | headless workers today; no scheduler UI | v0.6.0 |
| Parallel agents on one repo | part | worktree flag exists in Hermes, no UI | v0.6.0 |

## Models

| Capability | Studio | Notes | Plan |
|---|---|---|---|
| Claude subscription without an API key | yes | DirectSDK plugin, signed in through Claude Code | |
| Multiple providers | yes | 8 curated today | |
| Every provider Hermes supports | plan | 35+ from the provider registry, no app release needed | v0.5.0 |
| Live model catalog | part | Hermes caches today | v0.5.0 |
| New model badges and deprecation warnings | plan | catalog changelog in the Update Center | v0.5.0 |
| Model aliases | plan | `model.aliases`, written through the CLI | v0.5.0 |
| Fallback chains | plan | `hermes fallback` | v0.5.0 |
| Credential pools | plan | `hermes auth` add, label, reorder, reset | v0.5.0 |
| Multi-model mixtures | plan | `hermes moa` presets | v0.5.0 |
| Per-workspace model defaults | plan | folder pins a model | v0.5.0 |
| Automatic model selection | plan | deterministic, explainable, shadow-tested | v0.6.0 |
| Cost guardrails and budgets | plan | ships with routing | v0.6.0 |
| Per-model scorecards from real sessions | plan | success, retries, cost per task class | v0.6.0 |

## Memory and knowledge

| Capability | Studio | Notes | Plan |
|---|---|---|---|
| Local markdown memory as source of truth | yes | Obsidian vault, wikilinks, frontmatter | |
| Shared memory tools for agents | yes | `agentos-hub` read and write, search with ranking | |
| Cloud dashboard | part | Notion MCP today, page-level | |
| Two-way Notion sync with a ledger | plan | `notion_id`, `synced_at`, content hash | v0.4.0 |
| Show what was read and written per turn | plan | memory ledger on every turn | v0.4.0 |
| Project rules inherited by agents | yes | constitution, `CLAUDE.md`, `AGENTS.md`, `GEMINI.md` | |

## Engineering and delivery

| Capability | Studio | Notes | Plan |
|---|---|---|---|
| GitHub operations in the UI | yes | auth, repos, branches, commits, PRs, issues, Actions | |
| GitHub operations from chat | yes | `/gh` and "Ask Hermes" hand-off | |
| MCP server management across agents | yes | registry, native probe, catalog | |
| Computer use | part | Hermes toolset and cua-driver; no dedicated view | v0.7.0 |
| Per-tool permissions | part | per action today | v0.7.0 |
| Audit log | yes | every guarded action, terminal open, policy change | |
| Self-update with backup and rollback | yes | Studio, Hermes and Claude Code | |
| Signed builds and in-place updates | plan | Tauri updater with signed manifests | v0.7.0 |

## Transparency

| Capability | Studio | Notes | Plan |
|---|---|---|---|
| Live tool timeline | yes | cards plus worker and PTY output | |
| Reasoning panel | plan | Hermes stores reasoning; stream-json does not stream it | v0.4.0 |
| Token and cost per turn | plan | `state.db` plus `hermes insights` | v0.4.0 |
| Pause, stop, intervene | part | stop works; pause comes with `hermes pause` wiring | v0.4.0 |
| Router decision explanation | plan | "Auto chose X because Y" | v0.6.0 |

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
