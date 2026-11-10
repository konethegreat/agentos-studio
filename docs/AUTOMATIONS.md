# Automations: missions, roles and the review gate

The goal is hands off engineering across many repositories: work that starts itself, is done by a
team with distinct roles, is reviewed by a second agent before anything merges, and reports back
through the vault and Notion.

This document is the contract. It describes what a mission is, who does what, what stops a bad
merge, and what costs money.

## Cost model, stated first

Three providers are in play and they are not equal:

| Provider | Cost | Role in the fleet |
|---|---|---|
| Claude subscription | Flat, already paid | Default implementer and reviewer. Free at the margin, so it carries the volume. |
| Antigravity subscription (Argon) | Flat, already paid | Second opinion, refactors, large-context reading. |
| OpenRouter API | Metered, has a ceiling | Only when a task needs a specific model, and always inside a per-mission budget. |

Concurrency on the two subscriptions is not a cost problem, which is why the fleet can run several
missions at once. OpenRouter is the only meter, so it is the only provider that needs a hard stop.

## Objects

**Mission.** A durable unit of work, not a chat.

```
id, repo, goal, acceptance, role, status, provider, model,
budget_usd, spent_usd, task_id, reviewer, review_verdict, review_note,
created_at, updated_at
```

Status machine:

```
queued -> running -> review -> done
                 \-> blocked -> queued   (rework with the reviewer note)
                        \-> failed      (budget or repeated failure, human needed)
```

A mission is the only thing that can authorise a merge. A chat never can.

**Role.** A prompt template plus a permission set plus a model preference.

| Role | Does | May merge | Default provider |
|---|---|---|---|
| planner | Break a goal into tasks, write acceptance criteria | no | subscription |
| implementer | Change code, run tests, open a PR | no | subscription |
| reviewer | Read the diff and the tests, approve or reject | no | the other subscription |
| gatekeeper | Merge only an approved mission with green CI | yes, only with verdict approve | none, it is a policy check |
| memory | Turn an outcome into a decision, convention or gotcha | no | subscription |

**Review gate.** The reviewer is a different agent from the implementer, on a different provider
where possible. It reads the diff, the test output and the mission acceptance criteria, and returns
`approve` or `reject` with a note. Auto-merge is allowed only when all of these hold:

1. mission status is `review`,
2. the review verdict is `approve`,
3. the reviewer is not the implementer,
4. CI is green on the PR head,
5. the spend is inside the mission budget,
6. the repository is on the auto-merge allow list,
7. `hermes verify` did not fail in the repository (a repository with no build or test recipe
   passes this check).

Any one failing means the PR waits for a human. That is the whole safety story and it is
deliberately boring.

## Execution

- **Scheduler:** a tick every minute reads due missions and enqueues them, respecting a global
  concurrency cap and a per-repository cap of one running mission.
- **Event-driven bus.** With Automations on, the vault watcher reacts to every change under
  `02_Agent_Bus/`: within a few seconds it claims and starts each Pending task assigned to an
  installed headless agent (Hermes, Claude Code, Antigravity) that names a `Worktree:` of its own
  (every mission and handoff does), and it runs the scheduler tick so completed work moves to
  review. A task with only a `Repository:` line stays Pending: nobody watches a worker the watcher
  starts, and an unattended worker never runs in a shared checkout. Queue it as a mission, or use
  Run now in the Bus view while you watch it. Bursts of events become one pass (a 3 second gap, and an event
  during a pass reruns it afterwards). A task the watcher could not start is not retried from
  events in that session, because its failure note is itself an event; stuck detection covers it.
  The minute tick stays as the safety net, and passes never overlap (one tick at a time).
- **Hermes missions run on Hermes' kanban board.** Hermes owns execution: Dispatch runs
  `hermes kanban --board agentos-studio create` with the goal and acceptance criteria, the repository
  folder as the workspace (`dir:`), the implementer profile from MCP and Providers > Hermes, the
  mission's pinned model (set at creation, because the dispatcher claims a task within seconds) and
  the mission time cap (`--max-runtime`). The dispatcher inside the Hermes gateway runs the worker,
  so a mission keeps going with Studio closed. When the worker asks for review, kanban runs its own
  review pass and marks the task done; Studio treats that as "work finished", never as approval.
  The scheduler then records the metered spend of every Hermes session the task used, runs
  `hermes verify --json --skip-start` in the repository, and moves the mission to Studio review.
  A rejection queues the mission again and the next attempt is a new kanban task that carries the
  reviewer's note. The repository must be a Hermes project: a mission never runs in the home folder.
- **Parallel missions in one repository:** with "Hermes missions per repository" above 1, each
  Hermes mission gets its own git worktree (`--workspace worktree --project <repo>`, so
  `<repo>/.worktrees/<task>` on branch `<repo>/<task>-...`) and `hermes verify` runs in that
  worktree. Missions for other agents get a Studio worktree (below).
- **Claude Code, Antigravity and Qoder missions** keep the Agent Bus: a bus task completion with a
  summary and a PR URL moves them to review. Every bus task a mission writes carries a
  `Repository: <project folder>` line and the context files that exist (`03_Memory/SYSTEM_CONTEXT.md`
  and the repository's `README.md`, `CLAUDE.md`, `AGENTS.md`). A mission whose repository is not a
  known project with a folder does not start, and no worker ever runs in the home folder: a task
  with no `Repository:` line gets no worker, and the reviewer refuses a repository it cannot find.
- **Worktrees for every bus mission.** Each attempt gets `<repo>/.worktrees/<mission>-<attempt>` on
  branch `agentos/<mission>-<attempt>`, cut from the remote default branch. `.worktrees/` is added
  to the repository's local `.git/info/exclude`, so no tracked file changes. The task carries
  `Worktree:` and `Branch:` lines, the worker runs there, and the worktree is removed when the
  mission is done or failed (the branch stays, it is the pull request).
- **Workers inherit nothing.** Every headless worker starts with a cleared environment rebuilt
  from the allowlist on the Safety card (PATH as Studio's own search path, temp and profile
  folders, Windows basics), plus `AGENTOS_VAULT`, `AGENTOS_REPO` (the task's project, which scopes
  the hub's per-repo tool policy) and, for every start nobody watches, `AGENTOS_UNATTENDED=1`.
  API keys and tokens the app holds or reads never reach an agent: each CLI signs in through its
  own config (`claude`, `hermes`, `agy`, `gh`). An unattended start is refused with the reason when
  the task has no worktree or names one that no longer exists.
- **Workers start themselves.** `missions::start` (the board button and the scheduler alike) starts
  a headless worker as soon as the bus task exists, for every agent whose CLI is installed:
  - Claude Code: `claude -p` with `--permission-mode acceptEdits` and the repository's tool
    allowlist (see `ROUTING.md`).
  - Antigravity: `agy --input-format stream-json --output-format stream-json --mode accept-edits
    --settings <file>`, one turn on stdin, with the same allowlist written as Antigravity rules
    (`mcp(agentos-hub/*)`, `command(regex:git .*)`, ...).
  - Neither ever runs with `--dangerously-skip-permissions`.
- **IDE-only agents.** Qoder runs inside its IDE on this machine (its CLI, `qodercli`, is a separate
  install that is not present), and Antigravity counts as IDE only when `agy` is missing. Their
  missions are offered to them first. If the task is still unclaimed after the IDE pickup wait
  (Missions > IDE pickup wait, default 30 minutes, kv `missions.ide_wait_minutes`), the tick
  reassigns it to the best installed headless agent (Claude Code, then Antigravity, then Hermes),
  claims the task for it, writes the reason on the task and in kv `mission.note.<id>`, and starts
  its worker. With no headless agent installed the mission is blocked with the reason, never left
  running with nobody on it.
- **Stuck detection.** Every running mission has a heartbeat, checked on each tick:
  - Hermes: the newest task, run, event or comment time in `hermes kanban show --json` (which has
    no `last_heartbeat_at` field), and the last message of each worker session in `state.db`.
  - Headless bus workers: the process is alive in this app session, and the worker log, task file
    or worktree changed.
  - An IDE agent that claimed its task: the task file or any file in the worktree changed.
  No heartbeat for 15 minutes (2 minutes when the worker process is gone) means one automatic
  retry: `hermes kanban reclaim` for a running Hermes task, a dispatcher pass for one nobody picked
  up, a restarted worker for Claude Code and Antigravity, and a headless agent for an IDE agent
  that went quiet. Quiet again after the retry, the mission is blocked with the reason (for a
  Hermes task nobody ever picked up, the reason says to check that the Hermes gateway is running).
  The retry count is kept per attempt (kv `mission.retries.<id>`) and cleared when the mission
  starts again or reaches review.
- **Parallel missions per repository** applies to every agent now that every mission has its own
  worktree: with the setting at 1 a mission runs alone in its repository, above 1 up to that many
  share it.
- **Budgets** count only metered providers: the spend is read from Hermes usage data in `state.db`
  (worker sessions and Hermes reviewer sessions); subscriptions count zero. A Hermes mission whose
  metered spend passes its budget is blocked.
- **Time caps:** a per-task cap for Hermes missions (`--max-runtime`), and Hermes' own
  `agent.run_budget_seconds` for every Hermes run, set with `hermes config set`.
- **Review:** a second session, different provider, prompts for a verdict as JSON.
- **Keep awake:** while any mission is running, the app holds a Windows execution-state request so
  the machine does not sleep, and releases it when the queue drains.
- **Memory:** every finished mission calls `remember` with an `episode`, and a decision when the
  mission changed architecture. The ledger records what it read and wrote like any chat turn.

## Notion and Obsidian

- Each goal has a vault note (`01_Projects/Goals/<id>.md`) and a folder beside it with a note per
  mission and per decision and a JSON Canvas mission graph (`GOALS.md`, Obsidian views). Standalone
  missions have no note of their own; they are listed on the Notion missions page.
- Publish writes the decision summary, the latest digest, task status, missions, fleet and goals
  pages. Three things come back from Notion, each narrow: the Inbox database, proposals and cancels
  on the missions board, and proposals and Decision answers on the goals board (`NOTION.md`).
  Everything else Notion shows is owned by Studio and overwritten on the next sync.

## What must exist before a mission can merge anything

Per-tool permissions and an audit log already exist per action and per MCP tool. Since Phase G
the gatekeeper also runs the constitution gate on the pull request's added lines (dashes,
placeholder tags, hidden Unicode, credentials, docs changed with code) before the reviewer and
again before the merge, so the laws are checks rather than requests. Law 3 says the same thing
to the agents: inside a mission they open the pull request and stop, and only Studio merges.
Secrets move to the OS keychain before any unattended run, because a scheduled job must not read
API keys from a plain file. Signed builds and an updater matter less for correctness, more for
trust.

## Build order

1. **v0.5.0 (shipped):** mission store and status machine, role definitions, the review gate with
   its five conditions, and the Missions board in Mission Control.
2. **v0.5.1 (shipped):** the scheduler tick, keep-awake, concurrency caps, and automatic advance
   from bus completion to review. The scheduler never approves: it only moves work to the reviewer.
3. **v0.5.2 (shipped):** the reviewer as a real second session with a structured verdict, and the
   gatekeeper merge path.
4. **v0.6.0:** the model layer and per-session provider pins, so roles can name a model.
5. **v0.7.0:** per-tool permissions, keychain, signed builds, updater.
6. **v0.8.0:** the deterministic router, once missions have produced scorecards to learn from.
