# Self-healing fleet: the roadmap after 2.0

AgentOS Studio 2.0 finished the Beyond roadmap (B0 to B7). This roadmap closes the gap the owner
named after it shipped: models and providers are the weakest link, and when one misbehaves the
fleet cannot fix itself. The goal is a fleet that notices a broken provider, a stalled task or a
silent repository, repairs what it safely can, steers around what it cannot, and tells the owner
only what truly needs him.

Owner: Kone Tshivhinda, K-ONE IT SOLUTIONS. Written 2026-11-17 from the live fleet's records.

## Evidence (2026-11-17)

| Finding | Effect | Source |
| --- | --- | --- |
| Claude worker failures logged as `[Errno 32] Broken pipe` were classified `other` | No fix offered, no retry, provider shown as healthy | `provider.health` in `studio.db`, `errors.log` |
| Antigravity fallback replies "No reply" and "entitlement is exhausted" | 4 kanban tasks blocked for 4 to 8 days with `repeated_crashes` diagnostics | `hermes kanban diagnostics` |
| A finished task sat in kanban `review` for 25 hours | Its goal (G-028) and the next mission (M-097) stalled; the dispatcher logged `respawn_guarded: blocker_auth` every minute, which Studio counted as activity | `hermes kanban show t_1de25653` |
| 7 repositories opted in on GitHub never started a goal | The pass read the opt-in from a stale `origin/` ref and only fetched repositories already opted in | `autonomy.rs`, `git show origin/main:.agentos/routing.json` |
| 19 of 106 missions failed, every one "Stopped: E-n stop" | Infrastructure questions reached the owner and timed out into stop | `missions.review_note` |
| Vigil disallows squash merges | Its merge would have failed on every PR | GitHub GraphQL error |

## Phases

| Phase | Name | Ships as |
| --- | --- | --- |
| S0 | Honest health and the first repairs | 2.0.1 |
| S1 | Canary probes: know a provider is broken before a mission does | 2.1.0 |
| S2 | Fleet doctor: kanban diagnostics become actions | 2.4.0 |
| S3 | Self-built fallback chain | 2.3.0 |
| S4 | Troubleshooter agent for what rules cannot fix | 2.3.0 |
| S5 | Daily fleet health report | 2.3.0 |

## S0: Honest health and the first repairs (2.0.1)

- [x] S0.1 Broken pipes, timeouts and upstream 5xx are provider failures with a "Troubleshoot & fix"
  action (`provider_health::classify`).
- [x] S0.2 Auto-heal on every health check, cooldown 15 minutes per provider: enable a disabled
  plugin, refresh and reset credentials, learn spent models; mark a provider that cannot be
  restored degraded for 30 minutes so the quota grid steers to a healthy lane; log every action.
- [x] S0.3 Never claim a fix that did not happen: `hermes auth reset` exits 0 on an empty pool, so
  only a reset that touched a credential counts, and a transient failure with no fallback leaves
  the lane open for the mission retry instead of holding every mission.
- [x] S0.4 Transient worker deaths (broken pipe, connection reset, 502, 503, 504, overloaded) are
  retried instead of escalated (`automation::model_failure`).
- [x] S0.5 Merge fallback from squash to merge commit to rebase on `merge_method_not_allowed`.
- [x] S0.6 Opt-ins merged outside Studio are found: a repository that reads as not opted in is
  fetched again every half hour.
- [x] S0.7 A kanban review that never starts hands the work to Studio's reviewer after 30 minutes;
  `respawn_guarded` events are not counted as activity.

## S1: Canary probes (2.1.0)

Health today is read from logs after a mission has already failed. A canary asks first.

- [x] S1.1 Every 30 minutes, one tiny prompt per flat-rate lane (`hermes -z` with the lane's
  provider and model pinned, a fixed question with a known answer, 60 second cap). Never on a
  metered provider unless the owner turns it on (`canary::step`, `canary::run_probe_in`,
  `automation.rs`, unit tests in `canary::tests`, live test `live_canary`).
- [x] S1.2 Result per lane: ok, slow, wrong answer, malformed tool call, no reply, auth, quota.
  Two failures in a row close the lane in the quota grid with the reason; one success reopens it
  (`canary::record_in`, `quota_grid::lanes`, `QuotaGridCard.tsx`).
- [x] S1.3 A canary failure runs S0.2 auto-heal for that provider immediately, outside the cooldown
  (`provider_health::auto_heal_one_in`).
- Done when: verified 2026-11-18 by `live_canary::canary_closes_unreachable_lane_and_reopens_when_restored`
  on a throwaway profile (`outcome=no_reply` x2 closed lane and `quota_grid::steer` held/avoided it;
  subsequent `outcome=ok` in 18 s reopened it).

## S2: Fleet doctor (2.4.0)

Originally targeted for 2.1.0; the diagnostic, verdict and idle features landed after the 2.2.0
release, and automatic recovery through scheduler controls ships in 2.4.0. Live diagnostic
acceptance is the one item still open; its owner-facing status is recorded with that box below.

- [x] Diagnostic foundation: parse Hermes' diagnostics task array, match raw task IDs to Studio's
  `kanban:<id>` links, and report one advisory per task. Actual errors take precedence over generic
  recovery hints. Unknown, ambiguous, held and terminal mission links need review. Doctor requests
  no repin, reclaim, archive, credential repair, or provider probe. It is not strictly read-only:
  Hermes' diagnostics entry point may initialize, migrate or repair its own board database.
  Today warns about that boundary; isolated acceptance must account for those maintenance writes.
- [x] S2.1 Automatic recovery through the scheduler's existing policy and retry controls.
  The direct doctor mutation path was removed during stabilization: it bypassed pinned routes,
  retry budgets, owner holds and provider repair cooldowns. Recovery now runs only through
  `automation::recover_blocked`, which re-checks the fleet pause, requires the mission to be
  `blocked`, requires the recorded failure to be a model or provider failure, and then honors
  automations, the global and per-mission budget, the goal hold, the lane deferral and the
  `MODEL_RETRIES` budget before calling the existing `model_failure_retry`. Hermes reclaim is
  still never called and archive stays removed, so no worker is terminated and no worktree is
  removed. Routes and provider settings are never changed. `fleet_doctor::recover` re-reads the
  mission and refuses recovery if the `kanban:<id>` link changed. Covered by
  `automation::tests::recovery_queues_a_blocked_model_failure_within_the_retry_budget`,
  `automation::tests::recovery_holds_anything_the_scheduler_would_not_start`,
  `fleet_doctor::tests::blocked_model_failures_route_to_the_scheduler_retry` and
  `fleet_doctor::tests::doctor_requeues_a_blocked_model_failure_once_then_holds`.
- [x] S2.2 Non-terminal missions with no state change for 6 hours receive a verdict on Today.
  Verdicts describe recorded state and proposed checks, not unperformed heartbeat or PR checks.
  Invalid diagnostics, query errors or more than 300 missions produce a visible incomplete report,
  not an all-clear. Manual checks run off the desktop command thread; the diagnostics subprocess
  has a 30-second timeout. Doctor uses a strict mission query without changing the scheduler's
  existing tolerant query (`fleet_doctor::compute_stalled_verdicts`, `Today.tsx`).
- [x] S2.3 Report opted-in, unpaused registered Git repositories with no current goal,
  running/review mission, or goal start within the past hour. Failed/blocked mission updates and
  error log entries do not hide idle repositories. Ordinary project folders are excluded, while
  missing or broken repository paths and registry/state/query errors are reported. Local Git reads
  share a 15-second deadline with lazy fetching disabled. Unregistered repositories are not
  inspected by this pass (`fleet_doctor::compute_idle_repos`).
- [x] Acceptance: replay recorded blocked cards. `src-tauri/tests/fixtures/kanban-diagnostics-2026-11-23.json`
  holds the board rows read read-only on 2026-11-23: the two cards from the 2026-11-17 Gemini
  episode that were still blocked (`t_456f2788`, three crashed runs ending in `No reply:
  gemini-4-hydrogen-high didn't produce a reply`, and `t_9b07991f`, two consecutive failures ending
  in `entitlement is exhausted` for the same model) plus the three blocked `needs_input` cards
  (`t_5ab40e3d`, `t_15d6f0b8`, `t_7a10afcb`). `task_runs` and `tasks.last_failure_error` were read
  with `mode=ro&immutable=1`; no Hermes command ran. The 2026-11-17 cards that were resolved are no
  longer on the board, so this replay proves what the recorded rows still contain: the model
  failures become `retry_mission` and the scheduler queues them (`retried`), while the work and
  owner-input cards stay `needs_review` and keep their status
  (`fleet_doctor::tests::recorded_blocked_board_replays_with_only_model_failures_requeued`).
- [ ] Live diagnostic acceptance: run `live_fleet_doctor::isolated_diagnostics_produce_a_linked_advisory`
  under the live-machine lock after the pre-test backup. A caller must provision a `doctor-*`
  profile with a model-error task on its `agentos-doctor-test` board, then set
  `AGENTOS_DOCTOR_TEST_PROFILE` and `AGENTOS_DOCTOR_TEST_TASK`. The ignored test invokes diagnostics
  and checks the advisory; Hermes may perform maintenance writes on that isolated board. It does
  not create missions, write Studio state, or run a model. This is not automatic recovery acceptance.
  Status on 2026-11-23: not executed. The session implementing 2.4.0 runs beside the live fleet and
  its standing rule forbids invoking Hermes from a parallel session, so the test stays ignored until
  an owner-authorized run provisions the throwaway profile.

## S3: Self-built fallback chain (2.3.0)

- [x] S3.1 Build Hermes' `fallback_providers` from lanes the canary proves healthy, flat-rate
  first, through `hermes fallback` (never by editing `config.yaml`), and show the owner the change
  (`fallback_chain::build_chain`, `fallback_chain::sync_now`, `QuotaGridCard.tsx`).
- [x] S3.2 Demote a fallback that produces malformed tool calls or empty replies in long tool loops
  (the Gemini pattern) below one that does not, using canary and mission outcomes
  (`fallback_chain::collect_demoted`).

## S4: Troubleshooter agent (2.3.0)

- [x] S4.1 When rules find no fix, start a read-only Hermes session on a healthy lane with the
  failing provider's recent log lines, `hermes doctor`, `hermes auth status` and the kanban
  diagnostics, and ask for a diagnosis and one proposed command (`troubleshooter::run_for_provider`).
- [x] S4.2 Run the proposed command only if it is on a fixed allow list (`hermes auth refresh`,
  `hermes auth reset`, `hermes plugins enable`, `hermes kanban reclaim`, `hermes kanban
  dispatch`); anything else becomes an escalation with the diagnosis attached
  (`troubleshooter::is_allowed_command`, `troubleshooter::troubleshoot_with`).

## S5: Daily fleet health report (2.3.0)

- [x] S5.1 One message to the phone each morning: lanes open and closed, repairs made, missions
  stalled and why, repositories that moved (`fleet_report::step`, `fleet_report::build_report`).

## What this roadmap will not do

- It will not let a model choose or run arbitrary commands to repair the system. Repairs are a
  fixed set of reversible CLI actions; anything else goes to the owner with a diagnosis.
- It will not spend metered money to probe providers unless the owner turns that on.
- It will not edit Hermes' own files to repair it; every change goes through the Hermes CLI.
- It will not count a repair as done until the next check shows the provider working.
