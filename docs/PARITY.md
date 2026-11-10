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
| Approve dangerous commands from the chat | yes | over ACP: Hermes' five choices in a dialog; `chat -q` fallback denies them | |
| Stop a running turn | yes | ACP `session/cancel`; the partial reply stays | |
| Missions that run with the app closed | yes | Hermes missions run on `hermes kanban`; the dispatcher lives in the Hermes gateway | |
| Background and scheduled tasks | yes | Hermes cron jobs listed from `cron/jobs.json`; create, pause, resume, run and remove through `hermes cron` (MCP and Providers > Hermes) | |
| Hermes built-in memory and journey | yes | MEMORY.md, USER.md, `hermes memory status` and `hermes journey --json` in Memory > Hermes; edit and delete only through `hermes journey` | |
| Hermes usage report | yes | `hermes insights` on demand (MCP and Providers > Hermes) | |
| Supply-chain audit | yes | `hermes security audit --json` in Safety | |
| Mission notices to a phone | yes | `hermes send` to a target you set, when a mission is done or fails | |
| Parallel agents on one repo | yes | up to 4 missions per repository, each in its own git worktree and branch (kanban worktrees for Hermes, Studio worktrees for the rest) | |
| Headless missions for other agents | yes | Claude Code (`claude -p`) and Antigravity (`agy` stream-json) start themselves, with the repository's tool allowlist and never skipping permissions; the unattended profile rebuilds the worker environment from an allowlist so inherited keys do not reach it | |
| IDE-only agents | part | Qoder (no CLI installed here) is offered its missions first; unclaimed after the IDE pickup wait they go to a headless agent with a note | Qoder CLI when installed |
| Stuck mission recovery | yes | heartbeat per running mission; 15 quiet minutes means one retry, then blocked with the reason | |
| Agent identity on the shared bus | yes | per-agent token in each agentos-hub entry, `whoami`, agents write only as themselves, unknown callers read-only | |
| Event-driven task pickup | yes | the vault watcher starts delegated headless work within seconds | |
| Agent registry | yes | one card per agent with start mode, strengths, tools, cost model, quota headroom and health; the `agents` hub tool returns the same cards | |
| Handoff between agents | yes | `handoff` hub tool; Studio builds the packet and the receiver starts in the same worktree | |
| One agent asking another | yes | `ask_agent` hub tool: one short headless turn on Hermes, Claude Code or Antigravity, logged on the mission | Qoder when its CLI is installed |
| Multi-agent council | yes | two or three agents on different providers, ADR as a pull request through the same review gate | |
| Quota-aware agent assignment | yes | agent `auto`: constraints, measured success, then cost; defers instead of going metered | |
| Run timeline across agents | yes | one mission's path from creation to merge, with replay | |

## Models

| Capability | Studio | Notes | Plan |
|---|---|---|---|
| Claude subscription without an API key | yes | DirectSDK plugin, signed in through Claude Code | |
| Multiple providers | yes | every provider in Hermes' catalog, switched in Providers | |
| Every provider Hermes supports | yes | read from `provider_models_cache.json`; no app release needed | |
| Live model catalog | yes | Hermes' model cache, refreshed by Hermes | |
| New model badges and deprecation warnings | yes | Providers > Model layer: models new to Hermes' catalog this week, and a warning when a mission, folder default or fallback pins a model the catalog no longer lists | |
| Model aliases | yes | Providers > Model layer: `model_aliases` through `hermes config set/unset`; usable as `-m`, `/model` or a mission pin; alias keys stay in Hermes | |
| Fallback chains | yes | Providers > Model layer: order, add, remove; saved to `fallback_providers` with `hermes config set` | |
| Credential pools | yes | `hermes auth list`, priority, clear exhaustion, remove; adding a key runs `hermes auth add` in a terminal so the secret never passes through Studio | |
| Multi-model mixtures | yes | `hermes moa` presets shown; turn one on for every turn or off; configure in a terminal | |
| Per-workspace model defaults | yes | a folder pins a model for new Hermes chats; the deepest folder wins | |
| Automatic model selection | yes | opt-in per install: the router pins new Hermes missions with no explicit model or policy, only with 5 decided missions and a 5-point lead | |
| Cost guardrails and budgets | yes | per-mission budgets from Hermes usage data, metered providers only; time caps | |
| Per-model scorecards from real sessions | part | approval rate and cost per role, provider and model from missions | |

## Memory and knowledge

| Capability | Studio | Notes | Plan |
|---|---|---|---|
| Local markdown memory as source of truth | yes | Obsidian vault, wikilinks, frontmatter | |
| Shared memory tools for agents | yes | `agentos-hub` read and write, search with ranking | |
| Cloud dashboard | part | Notion publishing: digest, tasks, missions, fleet | |
| Two-way Notion sync with a ledger | yes | missions board: rows out per mission (hash ledger), proposals and cancels in; status, verdict and merge stay in Studio ([Notion](NOTION.md)) | |
| Goals and escalations from Notion | yes | goals board: rows out per goal with progress and the open question; goal proposals (Watch unless the author is trusted) and Decision answers in ([Notion](NOTION.md)) | |
| Show what was read and written per turn | yes | memory ledger per turn, including Hermes' own memory tool | |
| Project rules inherited by agents | yes | constitution, `CLAUDE.md`, `AGENTS.md`, `GEMINI.md` | |

## Engineering and delivery

| Capability | Studio | Notes | Plan |
|---|---|---|---|
| GitHub operations in the UI | yes | auth, repos, branches, commits, PRs, issues, Actions | |
| GitHub operations from chat | yes | `/gh` and "Ask Hermes" hand-off | |
| MCP server management across agents | yes | registry, native probe, catalog | |
| Computer use | yes | Work > Computer Use: toolset switch (approval), cua-driver status, install, update, doctor, and the action feed with screenshots ([Computer Use](COMPUTER-USE.md)) | |
| Per-tool permissions | yes | per MCP tool, enforced inside the agentos-hub server, plus per action in Studio; one policy per agent and per project as well, resolved repo > agent > global | |
| Audit log | yes | every guarded action, terminal open, policy change | |
| Self-update with backup and rollback | yes | Studio, Hermes and Claude Code | |
| Signed builds and in-place updates | yes | Tauri updater with a minisign-signed manifest; no code signing certificate (Windows shows unknown publisher) | |

## Transparency

| Capability | Studio | Notes | Plan |
|---|---|---|---|
| Live tool timeline | yes | cards plus worker and PTY output | |
| Reasoning panel | yes | ACP thought chunks in the collapsible Thinking panel | |
| Token and cost per turn | part | usage strip: context, session spend, compactions, credits | |
| Pause, stop, intervene | part | stop cancels the turn and approvals let you intervene; there is no pause | |
| Router decision explanation | yes | the reason (or why it declined) is shown on each mission it pinned | |

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

## Memory quality (v0.13.0)

| Capability | Hermes CLI | Studio |
|---|---|---|
| Notice a note that points nowhere | nothing; `recall` still returns it | checks every backticked path and symbol against the repository and reports the misses |
| Resolve two notes that disagree | a person notices in review | one strict turn per pair chooses the current note, and the other is marked superseded |
| Keep the history | edit or delete by hand | supersede flips frontmatter only; the old note stays on disk and ranks last in recall |
| Run it on a schedule | not offered | a weekly no-agent cron job for the stale half, and a Brain Quality panel for a run on demand |

## Skills from success (v0.13.0)

| Capability | Studio | Notes |
|---|---|---|
| Skill proposals from finished work | yes | three approved missions of one role and task class; the draft carries the ledger tool sequence, pitfalls and the missions as evidence |
| Create a skill from a local draft | yes | `hermes skills install` takes a registry identifier or URL only, so approval asks Hermes to create the skill from the draft and confirms it under the profile's skills folder |
| Dismiss a proposal for good | yes | `skills.dismissed.<hash>` in `studio.db`; the same group is not proposed again |

## Provenance (v0.13.0)

| Capability | Studio | Notes |
|---|---|---|
| A line names the mission that wrote it | yes | `git blame` then commit trailers, or the merged pull request mapped to a mission by `pr_url`; Brain form and a hover `why` in the GitHub diff view |
| Cost of the work behind a line | yes | measured spend, `flat` for subscriptions, `$0.00` for local models |
| Sessions behind a line | yes | the mission's Hermes sessions from the exchange log, with a link to the run timeline |
| Trailer block in mission merges | part | `trailers` and `parse_trailers`, and `review_agent::merge` appends the block to the squash body through `github::pr_merge_with` |

## Capability fabric (Phase G)

| Store | What Studio reads | What it never does |
|---|---|---|
| Hermes | `skills/<category>/<skill>/SKILL.md`; plugins from `plugins/.install-metadata.json` (name, revision, catalog tier and repo) | open the ACL-denied plugin folders, write to the skills tree |
| Qoder | `.qoder/skills/*/SKILL.md`; `.qoder/plugins/cache/<marketplace>/<plugin>/<version>` with `.qoder-plugin/plugin.json`, nested skills, agents and commands | change enablement in `settings.json` or `installed_plugins_v2.json` |
| Claude Code | `.claude/skills`; installed plugins under `.claude/plugins/synced` (installed) and marketplace clones under `.claude/plugins/marketplaces` (listed, not installed); `.claude/agents` | read app-managed enablement buckets, write any file |
| Studio bridge | `hermes/skills-external` (G7): junctions to Qoder, Claude Code or other skill folders, ranked after Hermes native skills; Hermes lists them as `external` | write into a bridged skill, hide a broken junction, bridge a name Hermes already has |
| Duplicates | one group per normalized skill name, precedence Hermes, bridge, Qoder, Claude, local; a copy whose hash differs from the preferred one is marked | merge or hide a copy |

## Capability packs (Phase G)

| Capability | Claude Code plugins | Qoder plugins | Studio |
|---|---|---|---|
| Manifest | `.claude-plugin/plugin.json`, unknown fields tolerated | `.qoder-plugin/plugin.json` | `capability.json`, unknown fields rejected, eight-word capability vocabulary |
| Signature and trust | marketplace trust only | marketplace trust only | Ed25519 over manifest plus content hash; official, verified, community tiers |
| What a pack may do | whatever its hooks and servers do | same | capped by tier, owner ceiling and the scoped tool policy, granted on a permission sheet and audited |
| Pinning and rollback | version in the marketplace | version cache | content-addressed store, lockfile with history, one-click rollback, hash mismatch refused |
| Install from | marketplaces | apphub | a folder or an https or file git URL, offline included |

## Evaluation and replay (v0.13.0)

| Capability | Hermes CLI | Studio |
|---|---|---|
| Replay finished work on another agent | nothing; a new mission starts from the current tree | `eval.run` replays the goal at the pull request's base commit in a detached worktree on `claude-code` or `hermes` |
| Compare a replay with the original | read the chat and guess | both runs scored on one rubric (tests plus reviewer verdict), with the original score read from `hermes verify` and `review_verdict` |
| Keep the replay out of production | the agent works in a real branch and can open a PR | detached worktree, no shell allowlist on Claude Code, `--ignore-rules` on Hermes, worktree removed on every path out |
| Leave a record | a chat session | one Markdown note per replay in `03_Memory/evals`, listed in the Brain view |
| Change a routing default from evidence | not offered | `router.rs` moves a default only after three of the model's own replays score higher than the original run (`with_eval_evidence`) |
