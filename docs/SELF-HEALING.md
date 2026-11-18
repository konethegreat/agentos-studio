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
| S2 | Fleet doctor: kanban diagnostics become actions | 2.1.0 |
| S3 | Self-built fallback chain | 2.2.0 |
| S4 | Troubleshooter agent for what rules cannot fix | 2.2.0 |
| S5 | Daily fleet health report | 2.2.0 |

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

## S2: Fleet doctor (2.1.0)

Hermes already diagnoses its board (`hermes kanban diagnostics`); nothing acts on it.

- [x] S2.1 Read the diagnostics every pass and map each kind to an action: `repeated_crashes` with
  a model error re-pins the task to the first open lane and reclaims it; `stuck_in_blocked` whose
  mission is already failed or deleted is archived; `repeated_failures` on entitlement runs S0.2
  for that provider (`fleet_doctor::act_on_diagnostics_with`).
- [x] S2.2 Any mission with no state change for 6 hours, whatever its status, gets a doctor
  verdict on the Today screen: what it waits on, and the action taken or needed
  (`fleet_doctor::compute_stalled_verdicts`, `Today.tsx`).
- [x] S2.3 A repository opted in with no goal started within an hour is reported with the reason
  (fetch failed, not a Hermes project, no default branch) (`fleet_doctor::compute_idle_repos`).
- Done when: the four blocked tasks of 2026-11-17 would each be retried, re-pinned or archived by
  the doctor in a replay test built from their real `kanban show` output.

## S3: Self-built fallback chain (2.2.0)

- [x] S3.1 Build Hermes' `fallback_providers` from lanes the canary proves healthy, flat-rate
  first, through `hermes fallback` (never by editing `config.yaml`), and show the owner the change
  (`fallback_chain::build_chain`, `fallback_chain::sync_now`, `QuotaGridCard.tsx`).
- [x] S3.2 Demote a fallback that produces malformed tool calls or empty replies in long tool loops
  (the Gemini pattern) below one that does not, using canary and mission outcomes
  (`fallback_chain::collect_demoted`).

## S4: Troubleshooter agent (2.2.0)

- [x] S4.1 When rules find no fix, start a read-only Hermes session on a healthy lane with the
  failing provider's recent log lines, `hermes doctor`, `hermes auth status` and the kanban
  diagnostics, and ask for a diagnosis and one proposed command (`troubleshooter::run_for_provider`).
- [x] S4.2 Run the proposed command only if it is on a fixed allow list (`hermes auth refresh`,
  `hermes auth reset`, `hermes plugins enable`, `hermes kanban reclaim`, `hermes kanban
  dispatch`); anything else becomes an escalation with the diagnosis attached
  (`troubleshooter::is_allowed_command`, `troubleshooter::troubleshoot_with`).

## S5: Daily fleet health report (2.2.0)

- [x] S5.1 One message to the phone each morning: lanes open and closed, repairs made, missions
  stalled and why, repositories that moved (`fleet_report::step`, `fleet_report::build_report`).

## What this roadmap will not do

- It will not let a model choose or run arbitrary commands to repair the system. Repairs are a
  fixed set of reversible CLI actions; anything else goes to the owner with a diagnosis.
- It will not spend metered money to probe providers unless the owner turns that on.
- It will not edit Hermes' own files to repair it; every change goes through the Hermes CLI.
- It will not count a repair as done until the next check shows the provider working.
