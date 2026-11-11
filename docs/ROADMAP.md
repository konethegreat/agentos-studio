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

## v0.12.0: the agents talk to each other (shipped)

Phase C of the 2026-11-08 master brief. Goal: the owner never copies text between tools. Agents
hand work, context and questions to each other through the hub, and every exchange is visible.
Depends on: Phase B (every agent runs headless in its own worktree, per-agent hub identity).
User guide: `docs/AGENTS.md`.

- Agent registry: one card per agent (how it starts, strengths, tools, cost model, quota headroom
  from `hermes usage`, measured approvals, health) on Work > Agents and through the `agents` hub
  tool; `studio.db` schema 9 adds the exchange log (`agent_events`) and `missions.kind` (#73).
- Handoff packets: hub tool `handoff`; Studio builds the packet (summary, next step, acceptance,
  diff so far, failing checks, recall, project brief) and the receiver starts in the same worktree
  with the packet as its first message (#75).
- Ask another agent: hub tool `ask_agent` runs one short headless turn on Hermes, Claude Code or
  Antigravity and returns the answer; a kv guard in `studio.db` stops ask loops (#76).
- Council for design decisions: two or three agents on different provider families answer in
  parallel, Hermes synthesizes an ADR committed as a pull request, with a decision note in the
  vault; Hermes `kanban swarm` was refused because it cannot seat other providers (#77).
- Quota-aware assignment: agent `auto` is assigned at start by hard constraints, measured success
  for the role, then cost; non-urgent work is deferred to the next window reset instead of going
  metered (#78).
- One timeline: Work > Run timeline shows a mission's path across agents with replay, built
  read-only from the exchange log, bus, worker logs, kanban, `state.db` and the memory ledger (#79).
- Fix: the merge gate resolves a Hermes project slug to `owner/name` before reading checks and
  merging (#81).
- Fix, found by the first Phase C live run: a mission's pull request is read only from the
  worker's completion summary, because a handoff packet's recall results can quote other pull
  requests; discarding a mission attempt deletes its branches even when the folder is briefly held
  (#85).
- Fixes from the release-build click-through: Replay keeps the current step in view, and quota
  reset times read as a short local date (#88).
- Also in this release (Phase E, Qoder): the Brain view shows the vault and Hermes memory stores
  side by side (#74), and finished missions write validated lessons, with repeated review
  blockers becoming one convention note per repository (#80).
- Verified live: `tests/live_handoff.rs`, `tests/live_ask.rs`, `tests/live_council.rs`, and the
  phase check `tests/live_phase_c_done.rs` against the running 0.12.0 release build (Claude Code
  asks Hermes, implements, hands off to Hermes, which documents and opens the pull request; a
  reviewer and the gate merge it; the timeline shows both agents, the ask, the answer, the handoff,
  the pull request and the review).

## v0.11.0: every agent can actually do the work (shipped)

Phase B of the 2026-11-08 master brief. The bus was wired to four agents but only Hermes ever ran.

- Every mission bus task names its repository (`Repository:` plus context files); a mission whose
  repository is not a known project does not start, and no agent works in the home folder (#66).
- Claude Code missions start themselves and finish: one git worktree per attempt, `claude -p
  --permission-mode acceptEdits` with the repository's `allowed_tools` from `.agentos/routing.json`
  (each `Bash` rule mirrored for `PowerShell`), never skipping permissions (#67).
- Antigravity runs headless (`agy` stream-json, the same allowlist as session rules); Qoder is IDE
  only on this machine (its CLI is not installed), so its missions are offered to it first and
  reassigned to the best headless agent after the IDE pickup wait, default 30 minutes (#68).
- Stuck detection: a heartbeat for every running mission; quiet for 15 minutes means one automatic
  retry, then blocked with the reason (#69).
- Per-agent identity on the hub: a token per agent in its agentos-hub entry, only the hash kept,
  `whoami`, agents write only as themselves, unknown callers are read-only; boot provisions
  missing tokens (#70).
- Event-driven bus: the vault watcher starts new headless work and moves finished work to review
  within seconds; the minute tick is the safety net (#71).
- Verified live: `tests/live_claude_mission.rs`, `tests/live_agy_mission.rs`,
  `tests/live_hub_identity.rs`, and the phase check `tests/live_phase_b_done.rs` against the
  running release build.

## v0.14.0: goal in, finished work out (shipped; both halves of the done check passed)

Phase D of the 2026-11-08 master brief, built by Hermes while Qoder prepares Phase H and Phase G
items 1 and 2 (`03_Memory/Phase_D_H_Coordination.md`). The owner states an outcome; Studio plans,
executes, reviews, merges, verifies and reports, asking only when a rule says it must. Design:
[GOALS.md](GOALS.md).

- **Goals and autonomy levels:** a goal is an outcome, its repositories, runnable done checks
  (command, URL, file), a metered budget, a deadline and an autonomy level (Watch, Ask on risk,
  Full; per repository default). Stored in `studio.db` schema 10 with a vault note per goal, shown
  in Work: Goals.
- **Planner:** a new goal is split by Hermes' own decomposer (`hermes kanban decompose --json`)
  into missions under the goal, each with its spec, acceptance criteria and dependencies (kv
  `mission.after.<id>`), agent `auto`. It runs against a private kanban database so nothing in a
  plan can start as a Hermes worker outside Studio's gate. At Watch the plan waits for Approve
  plan; otherwise the goal runs at once, and the scheduler starts a goal's missions once their
  dependencies are done (Watch: each waits for Start). Verified live: `tests/live_planner.rs`
  (2 missions in 39 s, no Hermes board touched).
- **Automatic review:** a mission entering review gets its reviewer without a click: the first
  installed agent on a different provider family from the implementer (Antigravity for Claude
  Code and Hermes on the Claude subscription, Claude Code for Antigravity), never the implementer.
  The reviewer now also sees the CI checks, the `hermes verify` result and the repository's
  conventions from memory. One review per round, one retry after a failure, then the mission is
  blocked with the reason. Goal missions always; standalone missions when Automations are on. The
  minute pass also runs while a goal is active. Verified live: `tests/live_auto_review.rs`
  (Antigravity judged Claude Code's pull request in 43 s).
- **Automatic merge:** approved work merges without a click when the level allows it (Full
  always, Ask on risk on the repository's merge allow list, Watch never) and every other gate
  condition holds: another agent approved, CI green with checks present, budget intact, `hermes
  verify` clean. Pending CI waits; a pull request merged or closed by someone else is recorded and
  left alone. Standalone missions only merge on their own while Automations are on.
- **Escalations, and only these:** a goal over its metered budget (the goal pauses), a second
  reject (the mission is held), a goal mission blocked after its retry (a destructive approval
  when the reason says so), a merge outside the allow list at Ask on risk. Each has a question,
  answers, a safe answer and a deadline (60 minutes by default, kv
  `escalation.timeout_minutes`); with no answer by the deadline the safe answer is taken. Answered
  in Work: Goals, under Waiting on you.
- **Self-healing CI:** a failing check on an approved mission's pull request has its failed jobs
  rerun once. Passing on the rerun makes it a flaky test: a gotcha note in the vault and a line on
  the timeline and in the digest, with no further retries. Failing again creates one fix mission
  under the goal with the failing checks and the end of the failing log, told to push to the pull
  request's own branch. Still failing after that fix escalates.
- **Goal completion:** once every mission of a running goal is merged (or its merge was decided
  by the owner), Studio runs the goal's done checks against the merged default branch in a
  detached worktree under its own data folder, never in the owner's checkout: commands with a
  10 minute limit, URLs that must answer 2xx, files that must exist. All passing: the goal is
  done with a report (missions, who reviewed each, spend, every check), an outcome note in
  `03_Memory`, an outcome line in `01_Projects/<repo>.md`, the Notion goals page republished
  (one line per goal; the goals database with a Decision column is Phase F) and the report sent
  to the messaging target. A failing check gets one fix mission; a second failure escalates.
- **Digest and the phone:** a morning and evening digest (kv `digest.times`, default 07:30 and
  18:00, set in Work: Goals, with Send now) leads with what waits on the owner, then what
  finished, metered spend, what runs, flaky tests and late goals. It is written to
  `04_Agent_Engine/digests/`, published as the Notion "AgentOS digest" page and sent through
  `hermes send` to the messaging target set in Providers: Hermes. Escalations go to the same
  target with a four-digit code; a reply such as `E-3 4821 merge` is read from the Hermes
  gateway's session store (read only) and applied when the code matches. Verified in Hermes
  v0.23.0: the gateway's own approvals already reach a paired platform with `/approve` buttons
  or text. WhatsApp was paired on 2026-11-09 (bus TASK-015, done) and the messaging target is
  the home channel, `whatsapp`.
- **Fleet pause:** Pause fleet in the status bar stops all new work in one click. Studio starts no
  mission, merges nothing, applies no escalation answer and creates no fix mission (kv
  `fleet.paused`), and Hermes' own `hermes pause` stops its cron, kanban and new gateway work.
  Running work finishes and still reaches review; escalations still expire to their safe answer
  and are applied on resume. Resume reverses both.
- **Done check, part 1 (passed live, #118):** `tests/live_planner.rs`
  `phase_d_done_a_goal_goes_from_one_sentence_to_merged_verified_and_reported` against the running
  release build. The test only creates the goal at Ask on risk from one sentence ("Add a probe()
  function to lib.js that returns { ok: true }, with a test in test.js, and document it in
  README.md") and plans it, as the Goals screen does. Hermes' decomposer made two missions, Hermes
  worked them, Antigravity reviewed them, Studio merged them (agentos-sandbox-1 #13 and #14), ran
  the three done checks on the merged default branch (`npm test`, a `node -e` probe check,
  `README.md` present) and wrote the report: 11 minutes, $0.00 metered, no escalation and nothing
  answered in Studio. The merges were reverted afterwards. The first runs found a real bug, fixed
  in the same PR: done-check commands with inner quotes broke under `cmd /C`.
- **Done check, part 2 (passed live, 2026-11-09):** an escalation reaches the phone and the answer
  is honoured (`phase_d_done_an_escalation_reaches_the_phone_and_the_answer_is_honoured`), run
  against the installed 0.14.0 app with WhatsApp as the messaging target. A goal at Ask on risk on
  a repository with `allow_merge` off: Hermes planned two missions, the reviewer approved them, the
  merge question for the first went to the owner's phone, the owner replied with the code and
  `merge`, the escalation read answered from the phone, Studio merged and the goal finished with
  every done check passing (28 minutes, most of it waiting for the reply). The merges were
  reverted afterwards. The run found a real bug, fixed in #123: deleting a goal kept the
  applied markers of its escalations, so a later escalation that got the same id would have been
  answered and never acted on.
- Known issue: `hermes verify` falls back to the owner's checkout when a mission's worktree is gone
  (#117, since 0.10).

Also in this release, built by Qoder in parallel (Phase H preparation and Phase G item 1):

- First run without a fleet (#100), install docs with SHA-256 checksums for every installer (#102;
  the checksum file now lists the names GitHub gives the assets, with dots for spaces), and the
  getting-started, glossary, troubleshooting and privacy pages (#104).
- Tool permissions per agent and per project (#116), described under v0.16.0 below.
- The two-job build cap for this 11.4 GB machine (#108): `.cargo/config.toml` `jobs = 2`, line
  tables only for dev and test builds.

## v0.15.0: Notion and Obsidian as real control surfaces (shipped)

Phase F of the 2026-11-08 master brief, by Hermes (#125, #126, #127). The owner can propose a goal,
follow it and answer its questions from Notion, and every goal has an Obsidian folder and mission
graph that work without Studio.

- **Goals board in Notion (F1).** `notion_goals.rs`: a database "AgentOS goals board" next to the
  missions board. Out, one row per goal (outcome, repos, done checks, autonomy, budget, status,
  progress, spend, the oldest open question and the report) behind the hash ledger
  (`notion-goal:<id>`); rows of deleted goals are archived. In, a goal proposal (Goal, Repos, Done
  checks) through `goals::create`, held at Watch unless its author is on `notion.trusted_authors`,
  and a Decision that answers the open escalation through `escalation::answer` (answered by
  `notion`) when the last editor is trusted. Status, checks, missions and merges never come from
  Notion. Synced every 5 minutes once the board exists, and from **Goals board** in Memory.
  Verified by `tests/live_notion_goals.rs` against the real workspace with a temporary studio.db.
- **Publish, do not mirror (F2).** Every board column has one owner (`OWNERS` in `notion_sync.rs`
  and `notion_goals.rs`, unit tested: Studio never writes a value into a Notion-owned column). The
  sync ledger now records the inbound direction too (`in` rows for proposals, Decisions, cancels
  and inbox notes), so a proposal row whose ID write-back failed is repaired instead of creating a
  second goal or mission. The digest page is published without its frontmatter (and says so when
  there is no digest yet); the digest and a finished goal refresh the goals board. Archiving a board forgets its ledger, so a later board no
  longer patches the archived rows. `notion_status` reports the ledger counts per direction.
- **Obsidian views (F3).** `goal_views.rs`: next to each goal note, `01_Projects/Goals/<id>/`
  holds a note per mission and per decision (escalation) with path wikilinks back to the goal,
  the missions it waits for, its pull request and its decisions, plus `<id>.canvas`, the mission
  graph in JSON Canvas (goal, missions by dependency column, decisions), coloured by status. The
  goal note gains Decisions, Pull requests and Mission graph sections. Regenerated whenever the
  goal note is written; only stale generated files are removed.
- Evidence: `tests/live_notion_goals.rs` passed twice against the real workspace with a temporary
  studio.db and vault (board created; unchanged sync free; the open question on the row; an
  untrusted Decision refused and cleared; an untrusted proposal created at Watch with the ID
  written back; a lost write-back repaired without a second goal; a disallowed answer refused; a
  trusted Decision answered E-1 from Notion; a deleted goal's row archived; the board archived and
  its ledger forgotten). The Obsidian views are covered by unit tests; the real studio.db had no
  goals at release time, so they are first written to the real vault by the next goal.
- Known issue: `hermes verify` falls back to the owner's checkout when a mission's worktree is gone
  (#117, since 0.10).

Also in this release, merged since 0.14.0:

- Qoder, Phase G item 2: the unattended worker profile and the Safety card (#120), described under
  v0.16.0 below.
- Hermes, Phase G item 2: the worker switch to that profile, with worktree-only unattended starts
  (#121), described under v0.16.0 below.
- Qoder, Phase H item 3: the Today home screen (#122), described under v0.17.0 below.
- Hermes: a reused escalation id never inherits an applied marker (#123), found by the 0.14.0
  phone check.
- One digest page: F2 had added a second "AgentOS digest" artifact next to the Phase D one, and
  the title-keyed ledger would have rewritten the page twice on every publish (found and fixed
  before the installed app updated).
- Help now bundles NOTION.md and COMPUTER-USE.md; installed builds had been missing both user
  guides because bundled docs are an explicit list.

## v0.16.0: safety for unattended work (in progress)

Phase G of the 2026-11-08 master brief, split between Hermes (items 3 to 5) and Qoder (items 1
and 2), per `03_Memory/Phase_D_H_Coordination.md`.

- **Tool permissions per agent and per project:** every MCP tool has a policy at three scopes,
  resolved repo > agent > global inside `mcp_stdio.rs` with the caller already resolved from the
  hub token. A denied tool is hidden from `tools/list` and refused in `tools/call`, and the refusal
  names the deciding scope. Repo overrides apply to sessions that declare `AGENTOS_REPO` (the
  launcher sets it per mission), so unrelated sessions keep their agent and global policies.
  Stored in `kv` under `perm.`, so no schema change, and every change is audited. Safety lists the
  real hub tools, hub agents and projects, and each tool shows whether a scope inherits or sets its
  own policy.
- **A stricter unattended profile:** a mission worker's environment is rebuilt from an allowlist
  (PATH, temp and profile folders), with the repo slug, `AGENTOS_UNATTENDED=1` and the vault path
  added and every other variable, API keys included, dropped. Safety shows one row per guarantee
  with the side that holds it: enforced here (`bus::worker_env`, `bus::unattended_refusal` and
  `permissions::unattended_env`) or waiting on Hermes (egress rules, `hermes secrets`).
- **The worker switch:** `bus::spawn_worker` now clears the child environment and rebuilds it with
  `unattended_env` for every headless worker, sets `AGENTOS_REPO` from the task's project, marks
  starts nobody watches with `AGENTOS_UNATTENDED=1`, and refuses those without an existing
  worktree. The bus watcher starts only tasks that carry a `Worktree:` line. Checked against
  Hermes v0.23 before designing the rest: `hermes egress` is iron-proxy (a TLS-intercepting
  credential-swapping proxy for sandboxes, off by default, not installed here) and `hermes
  secrets` pulls keys from Bitwarden or 1Password at startup; neither is an OS keychain, so those
  two guarantees stay open with that finding recorded.
- **The constitution gate (G4).** `constitution_gate.rs` judges the lines a pull request adds:
  no em or en dashes, no placeholder tags or stubbed macros, no hidden Unicode (tag characters,
  zero-width characters, bidirectional controls), nothing that looks like a credential (the
  finding never repeats the value), and a Markdown change whenever code under `src/` or
  `src-tauri/src/` changed. It runs before every reviewer turn, so a pull request that breaks a law
  goes back to its implementer with the findings through the normal reject path (recorded as
  reviewer `constitution-gate`, so a second reject still escalates), and again in the gatekeeper
  before any merge, on the diff as it is then. A diff too large to read in full does not pass.
  The line scanner is the core of the capability quarantine below.
- **A global metered budget with a hard stop (G3).** `budget.rs`: one limit per day, week or month
  (kv `budget.global_usd`, `budget.period`, set in Mission Control next to the other Automations
  settings) over the metered spend Hermes records in `state.db` for every session started in the
  period, chats included, subscriptions excluded. Once spent, the stop is hard for metered work
  only: no metered mission starts, a running metered Hermes mission is halted (its kanban task
  blocked and its worker's claim released) and blocked with the reason, and the owner gets one
  question per period (raise doubles the limit and releases what the stop blocked; the safe answer
  keeps the stop). Claude Code, Antigravity, Qoder and Hermes on a subscription keep working. The
  digest and its phone line show spend against the global budget and against each active goal's
  budget.
- **The policy accessor (G8 and G9 dependency):** `permissions::effective_tool_policy(agent, repo,
  server, tool)` returns the policy that applies to a hub call and a readable name for the rule that
  decided it. It resolves through `decide_scoped`, the function `check_scoped` enforces with, so a
  caller that must intersect a requested capability with the policy reads the answer the hub will
  enforce, without writing or prompting. The unattended rows for worktree, environment and inside
  the worktree now read `enforced`, because `bus::worker_env` and `bus::unattended_refusal` hold
  them; egress and secrets still wait on Hermes.
- **The MCP registry reader (G6 dependency):** `mcp_manager::catalog_entries(vault)` projects the
  eight catalog entries into `RegistryEntry` rows: id, name, description, transport, package,
  homepage, license, publisher and trust tier, all `official` for the servers this build ships.
  Metadata only: no parameter value, environment block or token is copied, so the capability
  fabric can index servers before any install path exists. `catalog()` keeps returning the rich
  install spec the MCP and Providers screens use.

Phase G grows into the capability platform (owner decision of 2026-11-10, bus TASK-018), built in
this order, one pull request each:

- **G3, budgets with a hard stop:** a global and per-goal metered budget that stops new work when
  passed, with spend against budget in the digest.
- **G5, one merge policy (done):** Law 3 of the constitution is now "Pull requests, one merge
  policy": agents open pull requests and AgentOS Studio merges them. A mission's agent never
  merges its own pull request; Studio runs the constitution gate, a different agent reviews, and
  Studio squash merges when the review approves, CI is green and the autonomy level allows it.
  Outside a mission an agent merges only when the owner directs that session to, after the
  builds and tests pass. The vault template, this repository's rule files and the generic
  `templates/CONSTITUTION.md` change in one pull request; the global rule files and every other
  fleet repository get the same text from the fleet broadcast afterwards.
- **G6, capability fabric (done):** a read-only index of every skill, plugin, agent and MCP server on the
  machine (Hermes, Qoder, Claude Code), with provenance, duplicate groups with a stated precedence
  (Hermes native, then the bridge, then Qoder, then Claude), a compatibility report per skill and
  a Capabilities view (Brain group). Nothing in a source store is written. `catalog.rs` reads
  `SKILL.md` frontmatter, `.qoder-plugin` and `.claude-plugin` manifests, agent and command files
  inside plugins, and Hermes' plugin install metadata (its plugin folders are not readable, so
  they are not opened). Every record carries a SHA-256 over its folder, the capabilities it asks
  for in the closed vocabulary of the trust pipeline (from `allowed-tools`, agent `tools`, bundled
  scripts, plugin hooks and MCP servers) and compatibility notes (unknown frontmatter fields,
  referenced `scripts/`, `references/`, `templates/` or `assets/` files that do not exist,
  absolute paths, hooks). Claude marketplace clones are listed as not installed. The MCP side
  reads Studio's own MCP catalog until Qoder's registry reader lands (TASK-019). On this machine:
  947 records, 70 duplicate skill groups.
- **G8, marketplace core and trust pipeline (done):** Studio's own `capability.json` manifest with a
  closed capability vocabulary, a content-addressed store with atomic install and rollback, a
  lockfile, trust tiers, a quarantine scan before activation and a permission sheet intersected
  with the tool policy. Lands before any install from a remote source. `marketplace.rs`: strict
  parsing (unknown fields and free-text capabilities rejected), Ed25519 signatures over the
  canonical manifest plus the content hash (official key `k-one-official-1`, verified publisher
  keys in kv, everything else community), staging with no symlinks and no `.git`, git installs
  from https or file URLs with no stored credentials, the quarantine scan, the permission sheet in
  Brain, Capabilities (community packs can never get shell, network, secrets or hooks; `mcp.tools`
  is checked tool by tool with `permissions::decide_scoped`), atomic activation under
  `%APPDATA%\agentos-studio\capabilities`, `capabilities.lock.json` with history, rollback,
  verify, remove, lockfile export per project, and an audit line per grant. Updates that add a
  capability need explicit consent; a hash mismatch is a refusal. Installing never runs pack
  content; hooks and bundled MCP servers run only through the extension runtime (G11). Two
  example packs in `examples/capabilities`. Full description in docs/CAPABILITIES.md.
- **G7, skill bridge (done):** a curated folder of junctions that Hermes reads through
  `skills.external_dirs`, so Qoder and Claude skills reach Hermes with provenance and rollback.
  `skill_bridge.rs` keeps `hermes/skills-external` with one `<source>--<skill>` junction per
  bridged skill (a hash-checked copy when a junction is impossible) and its provenance in
  `.agentos-bridge.json`. It refuses a skill Hermes already has natively (native wins), a second
  copy of a bridged name, a folder that links outside itself and a skill whose `scripts/` files do
  not exist; a broken junction is reported, not hidden. "Bridge to Hermes" on a skill in
  Capabilities; the Hermes config change (`skills.external_dirs`) is guarded, backed up and
  audited, or copied as a snippet. Verified through the Hermes runtime with one Qoder skill
  (`minimalist-ui`) and one Claude Code skill (`theme-factory`): both list as `external` in
  `hermes skills list` and a `hermes chat -s <skill>` turn quotes their first heading. The
  catalog now reads MCP entries through Qoder's `mcp_manager::catalog_entries` and skips Claude
  Code's `.trash`.
- **G9, agent packs (done):** persona, capability ceiling and model policy, enforced by the host;
  planner, code reviewer, security auditor, docs writer and researcher first. An agent pack is a
  capability pack of kind `agent-pack` with an `agent.json` (persona file, tools in Studio's
  agent-neutral vocabulary, disallowed tools, a model policy of `repo-routing`, `router` and
  `agent-default` with no model ids, a permission mode, guardrails, skills, the agents it is
  written for and whether it may run unattended). `agent_packs.rs` computes the ceiling: the
  pack's tools minus its disallowed ones, minus any whose capability was not granted at install,
  minus MCP tools the scoped policy denies (`permissions::effective_tool_policy` for that agent and
  repository); on Claude Code only the repository allowlist entries of the kept families remain
  and every dropped family goes to `--disallowedTools`, on Hermes the kept tools become `-t`
  toolsets. A pack runs unattended only when it opts in and the worktree, environment and inside
  guarantees are enforced. Choose a pack under "Agent pack" in the New mission form; the brief
  carries `Agent pack: <id>`, `bus::spawn_worker` applies the ceiling and puts the persona first.
  The Agents view lists installed packs with each ceiling and what was dropped and why. The five
  packs are in `examples/capabilities/agents`, signed with the official key.
- **G10, context ledger (shipped, `context_ledger.rs`, `ContextLedger.tsx`, migration 11):** Hermes
  and Claude Code keep doing their own compaction. Studio records each compaction in the
  append-only `context_events` table, writes an immutable 13-section anchored summary under
  `03_Memory/context/<agent>-<session tail>-<first>-<last>.md` with file, symbol, decision,
  masked tool output and commit pointers, keeps `.agentos/context/<mission>.md` in each worktree
  (excluded from git through `<git-common-dir>/info/exclude`, verified with `git check-ignore`,
  and named in the mission brief), redacts credentials through `constitution_gate::redact`
  before any note is written, supports non-destructive focused
  re-summarization (`-focus-<stamp>.md`), queries Hermes' FTS5 `messages_fts` alongside vault
  recall, and scores four probes (facts, artifacts, continuation, decisions) over 25 real
  sessions (38 cuts, 37/38 passing the 0.80 bar).
- **G11, extension runtime (shipped, `extension_runtime.rs`):** hooks, command packs and MCP
  bundles execute only through one module. Each run is refused unless the active copy matches the
  lockfile hash, the tier is official or verified, the kind's capability (`hooks.exec`,
  `shell.exec`, or `mcp.tools` plus `shell.exec`) is declared and granted, and `capability.run`
  passes `permissions::guard_in`. The child gets `env_clear()` plus the unattended allowlist
  without the home and app data variables, plus `AGENTOS_PACK_ID`; it runs in the named mission
  worktree or a deleted-afterwards sandbox, with a 30 second default and 300 second maximum
  timeout that kills the process tree, and capped, redacted output. MCP bundles register their
  server as `agentos-studio --extension-mcp <pack id>` (same scope, hash approved at
  registration) with `mcp.<server>.<tool> = ask` and `mcp.<server>.* = ask` unless the policy
  already denies a tool (`policy::decide_in` gained the `*` fallback). Runs, registrations and
  refusals are audit rows shown in Capabilities, Extension runtime, with the commands
  `extension_run`, `extension_register_mcp` and `extension_list_runs`. Examples:
  `examples/capabilities/hooks/commit-lint` and `examples/capabilities/mcp/text-stats`, both
  signed official. Also in this item, two G9 follow-ups from the qoder review: the agent pack
  ceiling keeps base `mcp__<server>` wildcards the pack does not exclude (the hub stays
  reachable), adds no MCP rule the repository allowlist lacks and denies dropped MCP tools by name,
  and `bus::spawn_worker` resolves the pack ceiling over one store connection
  (`agent_packs::for_worker_in`).

## v0.17.0: today at a glance (in progress)

Phase H wave 3 of the 2026-11-08 master brief. Today is the screen Studio opens on.

- **Waiting on you first:** open escalations, then goals whose plan is ready, in the same order as
  the digest. Each escalation is answered from its own options in place, with the safe answer kept
  as the quieter button, and an awaiting goal is approved from Today without opening Goals.
- **What runs and what it costs:** goals in progress with missions done over missions planned, a
  budget bar, the autonomy level and the spend; running missions and the worker processes this
  machine started, with agent and spend; metered spend across goals, with the note that flat
  subscriptions are not counted.
- **Last digest:** the newest note in `04_Agent_Engine/digests`, rendered as written through a
  read-only `digest_latest` command. Today polls every 15 seconds and has a manual Refresh.
- Today is read-mostly. It answers escalations, approves plans and resumes a paused fleet; every
  other action links to the screen that owns it.

## v0.13.0: memory that gets better with every mission (shipped)

Phase E of the 2026-11-08 master brief, built by Qoder in parallel with Phase C (PRs #83, #84, #86,
#87, #89 to #98). Memory becomes something that improves from finished work, with the owner
approving anything that reaches Hermes. The Brain view (#74) and the learning loop (#80) shipped
early in 0.12.0.

- **Learning runs on its own:** an approved mission extracts its lessons in the background, and the
  next mission's bus instructions carry the repository's active conventions (#92).

- **Memory quality (`memory_quality.rs`):** finds notes that point at paths which no longer exist
  and notes that cite a file or symbol `git grep -w` cannot find, then finds pairs of notes on one
  topic whose bodies contradict each other. One strict turn per pair decides which note is current.
- **Supersede, never delete:** the losing note keeps its bytes; only its frontmatter gains
  `status: superseded` and a `superseded_by` pointer, so recall ranks it last instead of losing
  history. Notes edited
  in the last 7 days are reported, not changed.
- **When it runs:** `agentos-studio --memory-quality --vault <vault> [--dry-run]`, a weekly cron job
  with no agent turn for the stale half (behind `hermes.cron.change`), and a Brain Quality panel
  that starts a run and shows what changed.
- `tests/live_memory_quality.rs` (ignored) creates the weekly job on a throwaway profile, finds it in
  `hermes cron list`, and runs the dry run against the real vault with every note's hash unchanged.
  Its first live run found that Hermes only accepts a script file name for `--script`; the weekly
  job now names its script instead of passing an absolute path (#98).
- **Skills from success:** three approved missions that share a role and a normalised task class
  produce one `SKILL.md` draft, with steps taken from the memory ledger's tool sequence, pitfalls,
  and the missions as evidence. Approve creates the skill through one Hermes turn behind
  `hermes.skills.create`; Dismiss records the group in `skills.dismissed.<hash>` so it is not
  proposed again.
- **Provenance (`provenance.rs`):** a line of code can name the mission that put it there, without
  reading the whole history. A mission merge appends a trailer block to its commit message
  (`Mission:`, `Agent:`, `Model:`, `Reviewer:`, `Cost:` where the cost is the measured spend,
  `flat` for subscription work or `$0.00` for a local model). `Goal:` waits for Phase D.
- "Why is this here?" (`file_provenance`): `git blame --porcelain` on one line,
  then the commit's trailers, then the merged pull request that carries the commit matched to a
  mission by `pr_url`. The answer shows the commit, mission, agent, model, reviewer, cost, PR and
  the mission's Hermes sessions, with a link to the run timeline.
- Brain has the form (repository, file, line); the GitHub diff view blames any new line with a
  hover `why` action against the local clone. A lookup that cannot run (no `gh`, no GitHub remote)
  is reported as a note, not an error.
- Verified live: `tests/live_provenance.rs`.
- **Evaluation harness (`eval.rs`):** takes a finished mission, reads its pull request's base
  commit, and replays the same goal and acceptance criteria on another agent or model in a detached
  worktree under `.worktrees/`. The replay never pushes and never opens a pull request.
- **Score:** half the repository's own test command (detected from package.json, Cargo.toml,
  go.mod or pytest), a quarter when no test command exists, and half the reviewer's verdict on the
  replay's local diff, using the same prompt and verdict parser as the review gate. The verdict is
  never written back to the mission.
- **Notes and comparison:** one note per replay under `03_Memory/evals/` with the base commit,
  duration, cost, tests and reviewer note, plus the original run's score on the same rubric. The
  Brain view has an Evaluations panel that lists them and starts a replay behind the new
  `eval.run` permission.
- **Router rule:** a model may become the router's default only when at least three of its own eval
  notes score better than the mission they replayed. Level or losing replays do not count. Until
  then the router declines and the shadow log says how many improving replays it has.
- `tests/live_eval.rs` (ignored) replays one finished sandbox mission and asserts the worktree,
  branch and routing policy are unchanged.
- Studio's own merge keeps the pull request number in the squash subject and refuses a merge it
  cannot attach trailers to (#97).
- Verified live, every done-when item of the brief: Brain view release-build click-through (#74);
  one convention note from two rejects, carried by the next mission; four lesson notes from one
  approval with no human step; the weekly job plus a dry run that changed nothing; a skill created
  from three successes inside a throwaway profile; trailers in a real squash merge that resolve back
  to the mission; one eval replay (75 against the original 75) with nothing left behind. Hermes
  turns ran on throwaway profiles only (`tests/live_phase_e_done.rs`, `live_memory_quality`,
  `live_skill_proposals`, `live_eval`, `live_provenance`).

## v0.10.2: cua-driver stays on the Hermes pin (shipped)

- Computer Use no longer offers the driver's own `update --apply`: it installed an unused copy, an
  elevated logon task, and a newer daemon incompatible with the version Hermes pins. Repair and
  doctor run `hermes computer-use install|doctor` instead. Found by the Phase A click-through.

## v0.10.1: safe to ship (shipped)

Phase A of the 2026-11-08 master brief. Everything since 0.10.0 (plan first, model layer, parallel
missions, model watch, ACP chat, aliases, router, Notion missions board, Computer Use) plus:

- ACP session lookup matches source, folder and start time, and polls state.db (#56).
- ACP turns report their duration (#57).
- Unanswered approvals are denied after a timeout, default 10 minutes (#59).
- Notion proposals wait for approval in Studio unless the author is trusted; Agent is validated
  (#60).
- The router needs 5 decided missions for the role in the last 60 days; the shadow line shows its
  real decision per role (#61, #63).
- Fixes from clicking every new screen in a release build: empty tool card after an approval,
  timeout field rounding, MoA off storing '', Computer Use doctor and update in PowerShell,
  refusal rows (#62, #63).

## v0.10.0: Hermes-native (shipped)

Principle: Hermes owns execution of Hermes work; Studio owns cross-agent orchestration (Hermes,
Claude Code, Antigravity), the review and merge policy, the vault and visibility. Do not rebuild a
Hermes feature; do not remove a Studio feature that covers non-Hermes agents.

- ACP spike (done, verdict not yet): `acp.rs` plus `tests/live_acp_spike.rs`. Approvals, cancel and
  resume work; skills preload, creation-time model and checkpoints, the `gui` source tag and
  structured tool cards do not. Chat stays on `hermes chat -q`. See [ACP spike](ACP-SPIKE.md).
- ACP as the chat transport with chat -q as fallback: deferred until the spike passes.
- Checkpoints on new Hermes sessions; undo through the Ink TUI `/rollback` (shipped, verified by
  `tests/live_checkpoints.rs`). Resumed turns no longer send creation flags Hermes refuses, and a
  skill picked mid-chat is invoked as `/<skill>`.
- Hermes missions through `hermes kanban` on the `agentos-studio` board (shipped). The dispatcher now
  runs inside the Hermes gateway (`hermes kanban daemon` is deprecated), so missions continue with
  Studio closed. Kanban's own review pass is not approval; Studio's reviewer and `merge_blockers`
  stay the only route to a merge. Claude Code and Antigravity missions keep the Agent Bus path.
  Verified by `tests/live_kanban_mission.rs`.
- Budgets from Hermes usage data, metered providers only (shipped). `--usage-file` only applies to
  `hermes -z`; the reviewer uses `chat --query-file`, so its cost is read from `state.db` by the
  session id Hermes prints. Time caps: `--max-runtime` per Hermes mission (per task, so it does not
  cap chats) plus `agent.run_budget_seconds` through `hermes config set` (shipped).
- `hermes verify --json` recorded on the mission before review; a failed verify blocks merge
  (shipped).
- Hermes built-in memory and `hermes journey` beside the vault in Brain, read-only (shipped). The
  memory ledger records Hermes' `memory` tool as target `hermes`, file MEMORY.md or USER.md.
- Surface cron, profiles (implementer and reviewer roles), `hermes send` on mission done or failed,
  `hermes security` in Safety and `hermes insights` as a panel (shipped, verified by
  `tests/live_hermes_surfaces.rs`).

## v0.10.1: plan mode and the model layer (shipped)

- Plan first in chat (shipped): Hermes `/plan` scoped to the folder, plan shown for approval, run
  on Approve. Verified by `tests/live_plan_mode.rs`.
- Fallback chains, credential pools, mixture-of-agents presets and per-folder default models, all
  through Hermes' own commands and config (shipped, Providers > Model layer). Verified by
  `tests/live_model_layer.rs`, which leaves config.yaml byte-identical.
- New model badges and deprecation warnings (shipped): Studio diffs Hermes' model cache against
  what it saw before; new models are flagged for a week, and a model that left the catalog is a
  warning when a mission, folder default or the fallback chain still pins it.
- Parallel missions on one repository through kanban worktree workspaces (shipped, MCP and
  Providers > Hermes). Verified by `tests/live_kanban_mission.rs`: two missions dispatched in one
  tick, each in `<repo>/.worktrees/<task>` on its own branch, both to Studio review.

## v0.10.1: Hermes chat over ACP (shipped)

- Shipped: ACP is the Hermes chat transport (Providers > Hermes > Chat transport), with sessions
  created through interactive `hermes chat` so model, skills and checkpoints are fixed at
  creation. Approvals in a dialog, Stop as `session/cancel`, reasoning in the Thinking panel,
  automatic `chat -q` fallback. Verified by `tests/live_acp_chat.rs`.
- Shipped: model aliases (Providers > Model layer), saved to Hermes' `model_aliases`; verified by
  a real `hermes chat -m <alias>` turn in `tests/live_model_layer.rs`.
- Shipped: the router can pick (opt-in, Missions > Let the router pick) under an evidence rule (5
  decided missions, 5-point lead), after an explicit choice and `routing.json`. Fixed on the way:
  `routing.json` was documented as filling blank missions but was never applied at creation.
- Shipped: two-way Notion missions board (proposals and cancels in, rows out, ledger); verified
  against the real workspace by `tests/live_notion_sync.rs`.
- Shipped: Computer Use view (Work > Computer Use); verified read-only against the real install by
  `tests/live_computer_use.rs`.

## Ideas worth considering

- Replayable runs: export a run (prompt, tool calls, results) as a Markdown report in the vault.
- Offline mode: detect no network and pin a local model automatically, with a clear banner.
- Team routing policies: shared JSON policy files in a repo, so a team agrees on model choices.

## Competitive notes

[`PARITY.md`](PARITY.md) tracks the capability targets this roadmap is measured against, and what we
deliberately do differently.
