# Orchestrator, Reports and Update Reliability - Design

Date: 2026-11-23
Status: approved for implementation (owner constitution: full autonomy, decide and record)

## Problem

1. The local app is correct (2.5.0) but the update path has real bugs: the StatusBar
   install never restarts the app and shows no progress, it re-checks only every 6 hours,
   two install paths compete, Studio updates ask for approval, and `hermes-plugins` can
   fail as a whole when one plugin is behind.
2. Today and Goals show "Waiting on you" cards: open escalations plus goals whose plans
   wait for a click. The owner does not want a hands-on queue at all.
3. There is no orchestrator. `automation::tick` and `autonomy::pass` are heuristics around
   `missions::start`; every mission still passes through review plus merge or escalation.
4. Work gets stuck: a mission in `running` with no `task_id` is skipped by the scheduler
   (automation.rs:411-413), paused missions are never resumed, and one retry is the whole
   retry budget.
5. The fleet is up to date but the app does not surface or adopt what the runtimes gained:
   Hermes 0.23.3 (prompt-cache lifecycle, compaction that archives dropped tool results,
   "Waiting on teammates" strip), Claude Code 2.1.330 (`/tasks` listing scheduled work,
   auto permission mode, runner queue metrics), Antigravity 1.5.15 (Orchestrator with
   Planner and Executor roles, `/swarm board` overview with blocked counts, auto-compaction
   override).

## Assumptions

- "Nothing requires my approval" means no routine approval prompts. The only human gate
  left is the Law 1 exception: irrecoverable production data, credentials, payments.
- The orchestrator merges only where Studio's existing merge policy allows it: reviewer
  approved, CI green, repository autonomy level allows. Otherwise it leaves the PR open
  and writes a report line.
- Applying CLI tool updates (Hermes, Claude Code, Antigravity, plugins) without a click is
  safe because they do not interrupt the running GUI. A Studio self-update still ends in a
  restart, so it stays a one-click "Restart to update", never a silent restart.
- The orchestrator never invokes a live agent runtime from inside Studio outside the
  existing headless worker path (`bus.rs`). No changes to Hermes CLI behavior.

## 1. Orchestrator

### Behaviour

New module `src-tauri/src/orchestrator.rs`, called from the existing scheduler thread
(automation.rs `start_scheduler`) once per minute, after `automation::tick`. It is a
policy layer, not a new runtime: it reuses `missions`, `goals`, `escalation`, `planner`,
`stuck`, `automation`.

Each pass, with `orchestrator.enabled` (kv, default `1`):

1. Dispatch: for every goal in `planning` with autonomy `watch`, switch it to `full` when
   the orchestrator owns it, then run `planner::plan_in_background`. For `ask` and `full`,
   automation already dispatches; the orchestrator only records the decision.
2. Route executors: choose the agent per mission with a pure function

   ```rust
   pub fn choose_executor(kind: MissionKind, repo: &str) -> &'static str
   ```

   rules: UI or frontend work goes to `antigravity`; refactors, reviews, multi-file Rust or
   TypeScript changes go to `claude-code`; CLI, cron, Notion, cross-repo work goes to
   `hermes`. `qoder` is never chosen for unattended work (IDE only). The choice is written
   to the mission before `missions::start` reads it, so Hermes kanban is unaffected.
3. Unstick: for each mission in `running` with `task_id` empty for more than 15 minutes,
   requeue it through `missions::unblock` under the existing retry budget, and write a
   report row. For each mission in `paused` whose pause reason is cleared and automations
   are on, requeue once; otherwise hold.
4. Resolve questions: for every open escalation on orchestrator-owned work, take the safe
   answer immediately (same rule `escalation::expire_due` uses), record the answer with
   `answered_by = "orchestrator"`, and write a report row that names the question, the
   answer and the reason. Exceptions that stay open: kind `destructive` when the question
   text matches production or credentials, plus anything raised by the Law 1 guard. Those
   are the only rows the owner ever sees in "Needs you".
5. Merge: for missions whose PR is approved and CI is green, call the existing
   `goal_loop::try_merge` path when `goals::merge_decision` returns `Merge`. When it
   returns `Ask`, the orchestrator answers `merge` escalations with "leave open" and
   reports the PR URL instead of blocking.

### Settings

kv keys: `orchestrator.enabled` (default `1`), `orchestrator.parallel_per_repo` (default
`1`), `orchestrator.auto_answer_minutes` (default `0`, meaning immediate). Exposed by
`orchestrator_status` and `orchestrator_set`. The orchestrator respects the fleet pause
and the global budget exactly as `automation::tick` does.

### Goals with an explicit hold

A goal created with autonomy `watch` stays `watch` and is not touched. It moves out of
Today into Goals as "Held by you". This is the only opt-out and it is per goal, never
global.

## 2. Reports

### Data model

New SQLite table in `store.rs` migration:

```sql
CREATE TABLE IF NOT EXISTS reports (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  at INTEGER NOT NULL,
  repo TEXT NOT NULL DEFAULT '',
  kind TEXT NOT NULL,
  title TEXT NOT NULL,
  body TEXT NOT NULL DEFAULT '',
  ref TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS reports_at ON reports(at DESC);
```

`kind` values: `mission.started`, `mission.reviewed`, `mission.merged`,
`mission.blocked`, `mission.unstuck`, `escalation.auto_answered`, `autonomy.pass`,
`digest.sent`, `fleet_report.sent`, `update.applied`. `ref` holds the mission id, goal
id, PR URL or product name. Rows are pruned to the newest 2000 by the same scheduler
pass.

New module `src-tauri/src/reports.rs` with `record(c, kind, repo, title, body, ref)`,
`list(since_id, repo, limit)` and `summary(hours)`. Writers: `missions` (start, submit,
block, merge), `orchestrator` (route, unstick, auto answer), `digest` and `fleet_report`
(after they send), `updates` (after an install succeeds).

### Commands and feed

Commands: `reports_list { since_id, repo, limit }`, `reports_summary { hours }`, and
`reports_mark_read { id }` which only moves a kv cursor (`reports.read_until`). Reports
are read-only; there is no accept or retry button in the view.

### UI

New view `Reports` in the Work group (`src/views/Reports.tsx`):

- Top strip: "Since you last looked" summary from `reports_summary`, one line per repo.
- Filter row: repository, kind, day; plain list below, newest first, grouped by repo.
- A report row shows time, repo, kind chip, title and body; PR URLs are links.
- Rail badge on Work shows the unread count from the cursor.
- Today links "N auto-handled" straight to Reports.

## 3. Today and Goals without "Waiting on you"

`src/views/Today.tsx`:

- Delete the "Waiting on you" card (lines 237-310) and the `waiting` badge (line 196).
- New Orchestrator card in its place: state (running, paused, holding), active missions
  with agent, elapsed time and last activity line, queued count, blocked count with next
  retry time, scheduled passes (digest times, weekly ship, update check, autonomy pass),
  and auto-handled-today count linking to Reports. This borrows the Antigravity `/swarm
  board` overview and the Hermes "Waiting on teammates" strip.
- A single "Needs you" line appears only for Law 1 items; with none, the card says
  "Nothing needs you. The orchestrator is running the fleet."
- Data sources: `orchestrator_status`, existing `missions()`, `goals()`, `workers()`,
  event `agentos://worker-stream`.

`src/views/Goals.tsx`: replace the "Waiting on you" escalation card (lines 307-320) with
the same "Needs you" line plus a read-only "Held by you" list of `watch` goals. The
escalations list stays read-only.

`src/components/ApprovalProvider.tsx`: unchanged for genuinely interactive actions the
owner starts (deleting a session, changing Hermes cron, rollback). The orchestrator never
runs through it, because it calls backend functions directly and never `permissions::guard`
for its own actions. Remove the guard from `update.apply` so a user-clicked update never
double-prompts; keep it on `update.rollback`.

## 4. Update reliability

Files: `src-tauri/src/updates.rs`, `src-tauri/src/commands.rs`, `src/components/StatusBar.tsx`,
`src/components/Updates.tsx`.

1. One install path. Delete `studio_update_install` (commands.rs:1148-1159) and its
   frontend API. StatusBar's button calls the Update Center apply for the `studio` product
   and navigates to Updates, showing "Downloading", then "Restart to update".
2. Restart after install. In `install_studio_release`, spawn the installer as a child,
   wait for it in a thread, then launch the installed exe
   (`%LOCALAPPDATA%\Programs\AgentOS Studio\agentos-studio.exe` on Windows, the bundle
   path elsewhere) and exit the app. No relaunch on installer failure; log a report row
   and keep the app open.
3. Progress. The install job already streams `agentos://update-progress`; the Update
   Center shows the last line and a spinner for the active product. A failed digest check
   keeps the app running and says which hash was expected.
4. Freshness. StatusBar and the banner read the same UpdatesProvider state (both are
   inside it, App.tsx:152-158). Add a re-check on window focus, keep the 3 hour cadence,
   and never show a stale version: the report carries `checked_at` and the button says
   "as of HH:MM".
5. Auto-apply policy. New kv `updates.auto_apply` (default `"cli"`): `cli` applies
   `hermes`, `claude-code`, `antigravity`, `qoder`, `hermes-plugins` automatically when an
   update is found; `all` adds Studio (still requires the restart click); `off` restores
   click-to-apply. A CLI product never prompts.
6. `hermes-plugins` partial failure. Update the plugin step to run per plugin and collect
   results; a product is `up_to_date` when at least one plugin updated and the rest are
   current, `failed` with the exact plugin names only when every plugin failed. The detail
   string lists which plugins changed.

## 5. Stuck-work fixes

- `automation.rs:411-413`: a `running` mission without `task_id` is judged by
  `stuck::judge` using the last heartbeat; when stale it is requeued once with
  `missions::unblock`, then blocked with a `stuck` escalation that the orchestrator
  auto-answers to "stop" and reports.
- Paused missions: orchestrator step 3 above; each resume writes a report row.
- Worktrees of missions that end in `blocked` after the retry budget are removed with the
  existing cleanup used on done and failed (automation.rs:549-558), so disk does not grow
  silently.
- No new retry count. The existing `MAX_RETRIES = 1` stays; the report row is the signal
  to redesign the mission, not another loop.

## 6. Research adoption summary

Adopted now:

- Executor routing with named Planner/Executor split (Antigravity Orchestrator).
- Orchestrator card with blocked counts and per-worker status (Antigravity swarm board,
  Hermes "Waiting on teammates").
- Scheduled-work visibility in the card (Claude Code `/tasks`).
- Auto-answer escalations with a recorded reason (Claude Code auto mode, Antigravity Auto
  mode asking less often).

Not adopted, with reason:

- Prompt-cache lifecycle, compaction archiving, auto-compaction override: owned by each
  runtime, not by Studio; Studio never manages model context.
- Swarms and agent teams: the fleet already dispatches per mission; parallel swarms inside
  one repo would fight the per-repo lock.
- Self-hosted runner metrics: Studio runs local workers, not remote runners.

## 7. Files

Create:

- `src-tauri/src/orchestrator.rs`
- `src-tauri/src/reports.rs`
- `src/views/Reports.tsx`
- `src-tauri/tests/orchestrator.rs` (isolated DB, no network)
- `src-tauri/tests/reports.rs` (isolated DB)

Modify:

- `src-tauri/src/store.rs` (reports table, migration)
- `src-tauri/src/lib.rs` (module list, commands, event if needed)
- `src-tauri/src/automation.rs` (no-task running branch, worktree cleanup)
- `src-tauri/src/goals.rs` / `goal_loop.rs` (orchestrator ownership, merge path reuse)
- `src-tauri/src/escalation.rs` (auto-answer entry used by orchestrator)
- `src-tauri/src/updates.rs` (single path, restart, auto apply, plugin partials)
- `src-tauri/src/commands.rs` (remove `studio_update_install`, add reports and
  orchestrator commands)
- `src-tauri/src/digest.rs`, `fleet_report.rs` (write report rows)
- `src/lib/api.ts`, `src/lib/studio.ts` (new calls, removed install)
- `src/views/Today.tsx`, `src/views/Goals.tsx`, `src/App.tsx`, `src/components/StatusBar.tsx`,
  `src/components/Updates.tsx`
- `README.md` and the affected docs

## 8. Testing

Backend, all offline and isolated:

- `orchestrator::choose_executor` mapping table.
- Auto-answer: open escalation on an owned goal is answered with the safe answer, kind
  `destructive` with production wording stays open; `answered_by = "orchestrator"`.
- No-task running mission requeues once then stops, with a report row each time.
- Reports: record, list filters, cursor, prune at 2000.
- Updates: plugin partial results, auto-apply policy selection, restart command
  construction (pure builder, no process spawn in tests).
- Existing suites: `cargo test -j 2` targeted binaries, `cargo clippy --all-targets -j 2
  -- -D warnings`, `npx tsc -b`, `npm run check:dashes`, `npm run test:guides`.

Frontend: Today shows no "Waiting on you"; the Orchestrator card renders running work and
the empty state; Reports filters and links; StatusBar update button routes to Updates.

## 9. Rollout

Single PR, merged after green CI (not a Studio mission, so the agent merges under Law 3).
Docs updated in the same PR. Then a 2.6.0 release, download site `sync:release` plus
`npm run deploy`, portfolio sync, and hub memory notes for the orchestrator policy and the
update path.
