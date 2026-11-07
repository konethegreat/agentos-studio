# AgentOS Studio roadmap

Phases are ordered by dependency, not by excitement. Each one lists what it needs from the
phase before it, so nothing is scheduled that cannot be built yet.

## v0.3.0 (shipped)

| Area | Ships |
|---|---|
| Hermes parity | Embedded PTY terminal (xterm.js + portable-pty) with tabs for PowerShell or your shell, interactive Hermes, Hermes TUI and Claude Code. "Continue in terminal" opens the current chat with `hermes --resume <id>` (or `claude --resume <id>`) in the same folder. |
| Updates | Hermes (`hermes update --check` plus release notes), Claude Code (npm registry plus CHANGELOG), Studio (GitHub releases). Checked at start and every 6 hours. In-app banner, OS notification, Update, Skip this version, Remind me later. Backups before every update, rollback from the Update Center. |
| Organisation | Workspaces > folders > sub-folders > chats in `studio.db`. Drag and drop, rename, pin, tags, search, archive. Folders map to directories and auto-collect chats run inside them, link to Hermes projects, and hold file attachments that are passed to the agent. |
| GitHub | Auth status, repos, branches, commits, PRs (diff, checks, review, merge, create), issues (create, comment, close), Actions (runs, failed logs, rerun). "Ask Hermes" hands any PR, issue or failed run to the chat. `/gh` slash command. |
| Safety | Per-action permission policies (allow, ask, deny) enforced in Rust, approval modal, audit log view. |
| Guidance | Command palette (Ctrl+K), tooltips on every new control. |

## v0.3.1: reliable startup (shipped)

- **Single instance:** a second launch focuses the existing window instead of starting a second
  heartbeat, watcher and fleet scan. The most common report of "it does not start" was an instance
  already running with no visible window.
- **Staged startup with safe self-repair:** eight bounded checks (folders, PATH, settings, database,
  vault, tools, Hermes config, agent registration) with per-step timings, streamed to a boot screen.
  Safe repairs run automatically; anything that changes another tool's config is offered as a fix.
- **Boot screen and Diagnostics view:** every check with its status and duration, the boot log, the
  crash log, and a re-run button. Panics now write `logs/crash.log` with a backtrace instead of
  exiting silently.
- **Deferred startup loads:** stack status, fleet scan and the board wait for the checks, so the
  window is responsive immediately.

## v0.4.0: projects, session branches and skills (shipped)

Hermes stays the source of truth; Studio connects to what it already owns.

- **Projects** (Hermes `projects.db`): a named workspace over one or more folders, with every chat
  that ran inside those folders, matched by the same longest-prefix rule Hermes uses. Create,
  rename, add a folder, archive and restore, all through `hermes project`.
- **Session branches**: a chat can be spun off from another one ("deal with separate things"),
  recorded in `studio.db` as a link from the child session to its parent. The child is a real Hermes
  session in the project's folder; Studio only remembers the relationship, so nothing is invented.
- **Skills**: the installed skills tree (name, description, category, version, tags) read from
  `SKILL.md` frontmatter, the full body rendered in-app, hub search and install through
  `hermes skills search|install`, removal and update. Any skill can be attached to a chat, which
  passes `-s name` to Hermes on every turn, so the agent loads it deliberately rather than by luck.
- **Chat header**: selected skills are visible and editable mid-conversation; a branch indicator
  shows when the next turn will be recorded as a child session.

## v0.4.1: memory, ledger and navigation (shipped)

- **Memory:** vault-first browser with wikilinks and frontmatter, and a "memory ledger" on every
  chat turn listing exactly which notes were read and written (assembled from `tool_use` events of
  `agentos-hub` and file tools). Notion is handled in v0.4.2 as publish-out, not as two-way sync.
- **Memory ledger:** every chat turn records what it read and wrote, and shows it under the message.
  The Memory tab lists indexed notes with type, status, inbound links and recent activity.
- **Transparency:** run timeline across chats, PTYs, workers and updates; reasoning panel fed from
  `state.db` `reasoning` columns after each turn (stream-json does not stream reasoning; the PTY
  does); token and cost panel from `state.db` and `hermes insights`; pause and step-in through
  `hermes pause` and the PTY.
- **Design system:** semantic tokens for colour, type scale, spacing, radius and motion; light
  theme; focus rings and a full keyboard navigation audit; reduced-motion support.
- **Guidance:** first-run onboarding, contextual guides, searchable help center built from the docs
  folder.

Why after v0.4.0: the memory ledger and cost panel produce the per-turn outcome data the model
router needs, and the design tokens are needed by every later screen, so both stay ahead of v0.5.0.

## v0.5.3: Notion as a publishing surface (shipped)

Two-way last-writer-wins sync was on this roadmap and has been removed. It sounds symmetric and is
not: the vault is markdown in git, Notion is a hosted database with its own editors, and a silent
last-writer-wins merge loses someone's work without ever saying so. Notion is better used where it
is strong, as the dashboard other humans read.

**Out (vault to Notion), written in Rust against the Notion API:**
- Fleet table: one row per repository with rule-file version, last sync and open tasks.
- Roadmap: the phase table from `docs/ROADMAP.md`, upserted by page id.
- Weekly decision digest: the week's `type: decision` entries, grouped by project.
- Task status: Agent Bus tasks mirrored as rows, status only.
- One owner per field. Each published field names its source; nothing is written back.

**In (Notion to vault), one narrow door:**
- A Notion database literally called "Inbox for agents". Pages created there are pulled into
  `00_Inbox/` with their title, body and page id, then left alone. No other Notion page is read.

**Bookkeeping:** `sync_ledger` (vault_path, notion_id, content_hash, synced_at, direction). A row
per published object, so a repeated publish with an unchanged hash is a no-op and a changed vault
note is visible as a diff before it goes out.

**Shipped:** `notion.rs` builds four artifacts from the vault (decision digest, task status,
missions, fleet), hashes each one, and skips anything unchanged through `sync_ledger`. With no
`NOTION_API_KEY` it runs a dry run and sends nothing. `notion_pull_inbox` reads the configured inbox
database into `00_Inbox`. `fleet.rs` still creates its bus tasks; that changes once a publish has
been observed against the live workspace.

## v0.5.0: missions, roles and the review gate (in progress)

The automation spine. See [`AUTOMATIONS.md`](AUTOMATIONS.md) for the contract.

- **Missions** as durable objects in `studio.db`, not chats: repo, goal, acceptance criteria, role,
  provider, model, budget, spend, status. Status machine `queued -> running -> review -> done`, with
  `blocked` for rework and `failed` for a human.
- **Roles**: planner, implementer, reviewer, gatekeeper, memory. Each is a prompt template, a
  permission set and a provider preference. Subscriptions carry the volume; OpenRouter is metered.
- **Review gate**: a second agent on a different provider reads the diff, tests and acceptance
  criteria, and returns approve or reject with a note. Merge is allowed only for an approved
  mission on an allow-listed repository with green CI and spend inside budget.
- **Missions board** inside Mission Control: create, dispatch, watch, review. Manual dispatch in
  this release; the scheduler follows in v0.5.1.
- **Identity**: installers carry the publisher K-ONE IT SOLUTIONS and the copyright holder, and the
  app credits Kone Tshivhinda in the rail and in the About text.

Why before the model layer: roles need per-mission provider and model values to exist before there
is anything to route. The database columns land here; the full provider catalog and routing land
after, and the router itself waits for mission scorecards.

## v0.5.1: scheduler, auto-advance and keep-awake (shipped)

- **Scheduler:** a pass a minute while the app is open. It advances running missions whose bus task
  is completed into review, and starts queued missions while there is room.
- **Concurrency caps:** a global cap (default 3, adjustable 1 to 12) and never two missions in the
  same repository at once.
- **Keep-awake:** the machine is held awake only while a mission is running, and released when the
  queue drains.
- **Controls:** Automations on or off and Run now in the Missions board. Turning automations off
  does not stop work in flight: running missions still finish and still go to review.
- **Rule that never bends:** the scheduler never approves. It only moves work to the reviewer.

## v0.5.2: reviewer session and the gatekeeper (shipped)

- **PR capture:** when a mission's bus task completes, the pull request URL in the worker summary is
  stored on the mission, so review and merge know exactly what to look at.
- **Reviewer session:** a second agent runs as a real one-shot session (`claude -p`, or
  `hermes chat --oneshot`) with the acceptance criteria and the actual `gh pr diff`. It must answer
  `{"verdict":"approve"|"reject","note":"..."}`; prose, an empty note or an unknown verdict counts as
  a failure and nothing is approved.
- **Gatekeeper merge:** merge runs only when the mission is done, the verdict is approve, the
  reviewer is not the implementer, `gh pr checks` is fully green, spend is inside budget, and the
  repository is allow-listed. Otherwise it returns the reasons instead of merging.

## v0.6.0: model layer, live catalog and per-mission pins (shipped)

- **Catalog from the runtime, not a hardcoded list:** providers and models are read from Hermes's
  `provider_models_cache.json`, with labels for the ones you use (Claude subscription, Antigravity
  Argon, OpenRouter, Anthropic, OpenAI, DeepSeek, Gemini, Nous, local). A model that appears in
  Hermes appears in Studio without a release.
- **Active provider and model** come from `config.yaml`, so the picker can show what is currently
  live rather than guessing.
- **Per-mission pins:** a mission carries a provider and a model. The Missions board offers the real
  model list for the chosen provider, with "inherit active model" as the default. This is the hook
  the router will use later; nothing is chosen for you yet.


Goal: anything Hermes can connect to in a terminal can be chosen in the app, and new models appear
without shipping a new Studio release.

- **Every Hermes provider, not a curated eight.** The provider list is read from Hermes's own
  provider plugin registry (35+ today: OpenRouter, Anthropic, OpenAI, Gemini, DeepSeek, xAI, Z.ai,
  MiniMax, Kimi, Qwen, Fireworks, Novita, NVIDIA, HuggingFace, Bedrock, Vertex, Azure, Nous, local
  and custom endpoints, and others). If Hermes gains a provider, Studio shows it with no code
  change. The curated cards stay as the short list; a full list sits behind "All providers".
  Trade-off: provider quirks vary, so each entry reports its own auth kind and readiness instead of
  pretending to be uniform.
- **Live catalog freshness.** Refresh from three sources on a schedule (at start, every 6 hours,
  and manually): the models.dev cache with ETag revalidation, each provider's `/v1/models` through
  `hermes model --refresh` semantics, and a local Ollama `/api/tags` probe. New models get a "new"
  badge, retired models get a deprecation warning on the card and in the chat header, and the
  catalog changelog is readable in the Update Center.
  Deliberate rule: freshness surfaces and suggests, it never silently changes your default.
- **Aliases, fallbacks, pools.** First-class UI for `model.aliases` (named model shortcuts),
  `hermes fallback` chains (tried on rate limit, overload or connection failure), `hermes auth`
  credential pools (add, label, reorder, reset exhaustion, refresh OAuth) and `hermes moa` presets
  (multi-model mixtures). All of it is written through the Hermes CLI so the terminal and the app
  never disagree.
- **Capability metadata per model.** Tool calling, vision, reasoning effort levels, context window,
  price per million tokens, streaming and structured output, from models.dev where known and
  provider documentation where not. This is the input the router needs, and it also lets the app
  refuse an impossible pairing early (for example, an image attachment on a model without vision)
  instead of failing mid-turn.
- **Per-workspace model defaults.** A folder (and anything under it) can pin a provider and model,
  so opening a repo always starts on the model that suits it.
- **Switch-cost awareness.** Warn before switching models inside a live session, because a switch
  invalidates the prompt cache and can double the cost of the next turn.

Depends on: nothing from v0.4 except the design tokens for the new panels.

## v0.7.0: trust and distribution

- Permission model extended to MCP servers and individual MCP tools (mirrors `hermes tools
  enable|disable`), HTTP APIs and computer use (`hermes computer-use status|doctor`), with
  per-workspace overrides.
- Studio secrets in the OS keychain (`keyring`), surfacing `hermes secrets` sources.
- Signed builds and the Tauri updater plugin with signed manifests, so updates install in place.
- GitHub notifications inbox and PR review comments inline on the diff.

## v0.8.0: auto model routing and cost guardrails

Goal: pick the right model for each task automatically, explainably and cheaply, with the user
always able to override. Design in [`MODEL-ROUTER.md`](MODEL-ROUTER.md).

- **Deterministic router first.** Hard constraints (must support tool calling, must fit the
  context, vision needed, provider allowed) filter the candidate set. A scoring pass then ranks
  what is left on task class, cost, latency and measured quality. Rules are inspectable and
  versioned; no model call is spent deciding which model to use.
- **Signals it uses:** the turn's task class (chat, code edit, review, research, long-context
  synthesis, vision, shell-heavy work), required tools and MCP servers, context size, attachment
  types, workspace pins and tags, remaining budget, and your own history with that task class.
- **Shadow mode first.** For the first weeks the router only recommends: every turn records what it
  would have chosen next to what actually ran, and a weekly report shows the difference in cost,
  latency and retries. Nothing changes until the numbers justify it.
- **Escalation ladder as an alternative to prediction.** Start on a cheap model, verify the result
  (tests, linter, self-check), escalate only if verification fails. For coding this is often more
  reliable than predicting difficulty up front, and it is cheaper per successful task.
- **Explainability.** Every turn shows "Auto chose X because Y", with a one-click pin to override
  for the turn, the chat or the workspace.
- **Cost guardrails, same release.** A monthly budget per workspace and overall, a warning
  threshold, and a hard pause on dispatch when exceeded. Auto routing without a budget is a way to
  spend money faster.
- **Model scorecards.** Per-model success rate, retries, tool errors, tokens and cost per task
  class, computed from your own sessions, shown on the provider cards and used by the scorer.
- **Evaluation harness.** A repeatable set of real tasks from your history, run against a candidate
  model or routing policy, with results stored in the vault so a prompt or model change can be
  compared instead of guessed.

Depends on: v0.5 capability metadata, and the cost and outcome data from v0.4.

## v0.9.1: session forking (shipped)

- **Fork at a message:** any message in a Hermes chat can be forked into a new Hermes session.
  At a reply, the fork keeps everything through that reply; at a user message, it keeps everything
  before it and returns the message to the composer for editing.
- **Written by Hermes, not by Studio:** the fork is a Claude Code style transcript imported with
  `hermes sessions import`, because the Hermes gateway is the only writer of `state.db`. Studio
  reads the new session back and refuses to report success unless every planned turn is stored.
- **Linked as a branch** in `session_links`, so Projects shows where it came from.
- **Rename through Hermes:** chat rename now calls `hermes sessions rename` instead of updating
  `state.db` directly, for the same reason.
- Limits, stated: tool outputs, reasoning and the system prompt are not copied; each tool call is
  kept as a `[ran tool: name]` line. A resumed fork runs on the active model.
- Verified by `tests/live_fork.rs` against a real session (fork, read back, compare, delete).

## v0.9.1: contextual guides (shipped)

- **One tip at a time, from your own state:** a chat nearing its context limit or already
  compacted, a reply running past a minute, a new chat about to start in the home folder, a chat
  long enough to fork, an empty Missions board, queued missions with Automations off, a mission
  waiting for review, an empty memory index. Each tip names the action and offers it as a button.
- **Gone when handled:** Got it, or doing what the tip suggests, retires it for good (`studio.db`
  kv `guides.dismissed`). Help has a Tips switch and Show tips again, which also restores the
  first-run guide.
- **Read more opens the right doc:** tips link to `docs/CHAT.md` and `docs/AUTOMATIONS.md` in Help.
- **Help works in an installed build:** `docs/*.md` ships as a Tauri resource and is resolved from
  the resource folder when no checkout is present.
- Rules are pure (`src/lib/guides.ts`) and tested in CI with `npm run test:guides`. The full list is
  in [`GUIDES.md`](GUIDES.md).

## v0.9.1: design tokens and a light theme (shipped)

- **Semantic tokens** for surfaces, text, status, accent, type scale and motion, in one file. Every
  hex value and pixel font size in the components was replaced by a token, 1:1, so dark is unchanged.
- **Light theme**, with Follow Windows, Dark and Light in the status bar, applied before first paint.
  The neutral ramp and pale accent shades are theme aware, with light values matched to the dark
  contrast. Code blocks and the terminal follow the theme.
- **Focus and motion:** a visible keyboard focus ring everywhere, and reduced-motion support.
- **Audit:** all fourteen screens captured in both themes; dark pixel-diffed against `main`. See
  [`DESIGN.md`](DESIGN.md).

This completes the design-system bullet listed under v0.4.1, which was never built at the time.

## v0.9.2: the hub agents run follows the app

- **Versioned hub binaries:** Update agentos-hub copies the app's own exe to
  `%LOCALAPPDATA%/AgentOS Hub/<version>/agentos-studio.exe` and registers that copy in Hermes,
  Claude Code, Qoder and Antigravity. The app's install folder is never registered, so the installer
  and updater can always replace it, and a new version never has to overwrite an exe that running
  sessions hold open. Older version folders stay for the sessions still using them.
- **Hermes keeps its settings:** when only the command changes, the registration is updated with a
  single `hermes config set` instead of a remove and re-add.
- **Visible drift:** MCP and Providers and the startup checks say when agents run a hub other than
  this app version, and offer the update.
- Before this, Hermes and Claude Code ran a 0.4.0 hub from the build folder, and Qoder and
  Antigravity still ran the Python `agent_bus_server.py`. The Rust hub provides every tool the
  Python server had, plus recall, remember and project_brief.
- Verified by `tests/live_hub_cutover.rs` against the real agent configs.

## Ideas worth considering

- Replayable runs: export a run (prompt, tool calls, results) as a Markdown report in the vault.
- Offline mode: detect no network and pin a local model automatically, with a clear banner.
- Team routing policies: shared JSON policy files in a repo, so a team agrees on model choices.

## Competitive notes

[`PARITY.md`](PARITY.md) tracks the capability targets this roadmap is measured against, and what we
deliberately do differently.
