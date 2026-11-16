# Beyond 1.3: the fleet that does not wait

AgentOS Studio 1.3.0 can take a goal, plan it, build it in isolated worktrees, review it with a
second agent and merge it. This roadmap is what comes after: a fleet that keeps working when the
owner is away, learns from every run, proves what it did, improves itself and, finally, takes work
from paying clients.

Owner: Kone Tshivhinda, K-ONE IT SOLUTIONS. Written 2026-11-16 from the live fleet's own records.

## Where 1.3.0 actually stands (evidence, 2026-11-16)

| Measure | Value | Source |
| --- | --- | --- |
| Missions run | 97 | `studio.db` missions |
| Approved by a second agent and merged | 75 | `review_verdict = approve` |
| Rejected | 1 | `review_verdict = reject` |
| Failed | 19 | every one `Stopped: E-n stop` |
| Metered spend | $0.00 | flat subscriptions did all of it |
| Escalations raised | 16 | `escalations` |
| Escalations answered by the owner | 13 | `answered_by = studio` |
| Escalations that expired into "stop" | 2 | `answered_by = timeout` |

The finding that shapes this roadmap: **no mission has failed because the code was wrong.** Every
failure is an escalation that was answered "stop" or that nobody answered, and each one cancelled
its whole goal (up to four missions). The owner is the bottleneck, and the questions he is asked
are mostly about infrastructure (a worker lost, a provider out of quota), not about the work.

Three cracks found while writing this:

1. A goal whose planner hits an LLM error stays in `planning` forever (G-029).
2. A worker that dies because its fallback provider is out of entitlement is escalated to the owner
   instead of being retried (E-16, M-096).
3. Closing the window quits the app, and with it the scheduler: the fleet stops until the owner
   opens Studio again.

## Build order

Each phase depends only on the ones before it. Phases ship as minor releases; every task ships as
its own PR with the full gates.

| Phase | Name | Ships as | Depends on |
| --- | --- | --- | --- |
| B0 | Foundation: nothing waits on a crash | 1.4.0 (shipped) | 1.3.0 |
| B1 | Shadow Kone: the owner's judgment, delegated | 1.4.0 (shipped) | B0 |
| B2 | Quota grid: every subscription used, none exhausted | 1.4.0 (shipped, two tasks left) | B0, router scorecards |
| B3 | The dreaming brain | 1.6.0 | B0, memory ledger |
| B4 | Time machine: counterfactual replay | 1.7.0 | eval replays, B2 |
| B5 | Proof-carrying work | 1.8.0 | B4 |
| B6 | Ouroboros: Studio builds Studio | 1.9.0 | B1, B5 |
| B7 | Agency in a box | 2.0.0 | B1, B5, B6 |

## B0: Foundation, nothing waits on a crash (shipped in 1.4.0)

Goal: no goal stalls or dies because of infrastructure.

- [x] **B0.1 Provider and worker failures are retried, not escalated.** Extend
  `automation::model_failure` to recognise an exhausted entitlement or billing limit, a lost managed
  worker (`EOFError`), a gateway connection failure and a worker that exited without reporting.
  Done when: the E-16 reason text is classified as a model failure in a unit test.
- [x] **B0.2 A failed plan is planned again.** `goal_loop` replans a goal left in `planning` with
  "The planner failed" after 5, 15 and 45 minutes, then escalates once. Done when: a unit test walks
  the backoff, and G-029 replans on its own in the running app.
- [x] **B0.3 The fleet keeps running with the window closed.** Closing the window hides it to the
  system tray; the tray menu has Open, Pause or resume the fleet, and Quit. Start at login moves to B2, where the night shift needs it.
  Done when: the window is closed and a scheduler pass still runs (timeline entry after close).
- [x] **B0.4 This roadmap in Help.** `docs/BEYOND.md` is bundled and linked from the README.

## B1: Shadow Kone, the owner's judgment delegated (shipped in 1.4.0)

Goal: zero missions lost to silence, without letting an agent take a decision the owner would
not have taken. Design: `docs/SHADOW.md`.

- [x] **B1.1 Decision memory.** Every escalation answer is a precedent: kind, reason class (pure
  classifier: provider, worker lost, no PR, no heartbeat, CI, reject, merge allow list, budget,
  destructive), repository, agent, answer, who answered. Read from `escalations`, no new table.
- [x] **B1.2 Learned rules.** A reason class with at least 3 owner answers, all the same, becomes a
  rule. One contradicting answer removes it. Destructive and budget questions never get a rule.
- [x] **B1.3 Veto window.** When a rule matches, Shadow announces the answer on Today and the phone
  ("Shadow will retry M-096 in 10 min. Reply E-16 <code> stop to overrule") and answers when the
  window closes, `answered_by = shadow`. The owner's answer inside the window always wins.
- [x] **B1.4 Shadow screen.** Autonomy tab section: on or off, window length, the learned rules with
  their precedents, Shadow's answers and the owner's overrules, agreement rate.
- [x] **B1.5 Self-demotion.** An owner overrule is itself his newest answer, so it breaks the streak
  and retires the rule at once (stricter than the two overrules first planned); three new
  consistent answers bring it back.
- [ ] **B1.6 Judge for novel questions.** For a question with no rule, a Claude turn with the
  constitution and the nearest precedents proposes an answer with a confidence; only reversible
  answers (retry) above 0.8 are announced, everything else waits for the owner.
- [ ] **B1.7 Voice escalation (opt-in).** Urgent questions near their deadline go out as a phone call
  through the `hermes-phone` plugin, with the owner's explicit per-call approval as the plugin
  requires.

## B2: Quota grid (shipped in 1.4.0; B2.6 and B2.7 open)

Goal: every flat-rate subscription is used around the clock and no worker is ever sent to a provider
that is out of quota.

- [x] B2.1 Live headroom per subscription. Agent cards already read `hermes usage --json` for Claude
  and Hermes; the Antigravity card now reads the same Antigravity subscription
  (`antigravity-oauth`) instead of "reports no quota".
- [x] B2.2 Entitlement memory: a worker error naming a model out of credits or entitlement
  (`entitlement is exhausted for <model>`, the real E-16 text) closes that model for 5 hours
  (kv `quota.exhausted.<model>`), with a note on the mission's timeline.
- [x] B2.3 Steering: the lanes are the owner's own Hermes routes, primary then fallback chain
  (`quota_grid.rs`). A Hermes mission with no pinned model starts on the primary when its lane is
  open, otherwise this attempt is pinned to the first open fallback. Agent choice for `auto`
  missions already bids on live headroom and measured success (`assign.rs`).
- [x] B2.4 Hold instead of fail: when every route is closed, the mission waits for the soonest
  reset (a deferral the scheduler already honours) instead of starting a worker on a dead lane.
- [x] B2.5 The quota grid card on the Agents screen: each route, open or closed, its headroom and why.
- [ ] B2.6 Start at login, so the night shift starts after a reboot without the owner.
- [ ] B2.7 Forecast: when each subscription frees up, and heavy missions held for the hours after a
  reset.

## B3: The dreaming brain (1.6.0)

Goal: what the fleet learns reaches the next worker.

Evidence (2026-11-16): the learning loop wrote 280 notes in three days (131 conventions, 111
gotchas, 55 to 83 per repository) with only two near-duplicate pairs. The notes are good; the gap
is delivery. An implementer receives a 1,500-character conventions block, so most of what a
repository has learned never reaches its next worker.

- [ ] B3.1 Nightly distillation: per repository, rank the lesson notes by use (recall hits,
  rejections they would have prevented, recency) and write one generated playbook note
  (`01_Projects/<repo>/Playbook.md`) that fits the implementer's context budget.
- [ ] B3.2 Merge near-duplicates and mark contradictions for the owner instead of keeping both.
- [ ] B3.3 Morning card on Today: what the fleet learned yesterday and which playbooks changed.
- [ ] B3.4 Proof of learning: first-attempt approval rate and retries per repository, before and
  after each playbook, charted on the Brain tab.

## B4: Time machine (1.7.0)

Goal: any past mission can be rerun with another agent, model or prompt, and every real mission
becomes a benchmark.

- [ ] B4.1 Fork a finished mission at its base commit into a sandbox worktree; never pushes.
- [ ] B4.2 Side-by-side result: diff, tests, review verdict, time and cost for each variant.
- [ ] B4.3 Automatic benchmark set: approved missions with a done check become eval cases.
- [ ] B4.4 New model day: when `model_watch` sees a new model, replay the benchmark set on it and
  post the comparison.

## B5: Proof-carrying work (1.8.0)

Goal: every merge carries a receipt anyone can verify.

- [ ] B5.1 Receipt per merge: mission, PR, head commit, CI run ids and results, reviewer and verdict,
  done-check output, `prove-it` probes, all hashed.
- [ ] B5.2 Hash chain over receipts, signed with the Studio minisign key already used by the updater.
- [ ] B5.3 Public ledger on the website: totals, per-repo receipts, a verify page.

## B6: Ouroboros, Studio builds Studio (1.9.0)

Goal: the fleet improves AgentOS Studio itself, with the owner approving releases only.

- [ ] B6.1 Opt agentos-studio into autonomy with protected paths (signing, release workflow, updater
  keys, constitution) that no mission may touch.
- [ ] B6.2 Candidate release: when a phase completes, the fleet builds a release candidate and runs
  the live mission templates on it.
- [ ] B6.3 Weekly ship message: changelog, receipts and test totals to the phone; "ship" tags it.

## B7: Agency in a box (2.0.0)

Goal: K-ONE IT SOLUTIONS can take a client's goal and deliver a reviewed pull request with proof.

- [ ] B7.1 Intake on the website: goal, repository, deadline.
- [ ] B7.2 Quote from scorecards and history (time, confidence), approved by the owner on the phone.
- [ ] B7.3 Client repository through a GitHub invitation; work on the fleet like any goal.
- [ ] B7.4 Delivery: the PR, its receipt and a client report.

## What this roadmap will not do

- No agent ever answers a destructive, budget or merge-allow-list question on the owner's behalf.
  Shadow handles the questions the owner has already answered the same way, and he can overrule it.
- No model call picks a model (the router rule stands). Bids come from recorded outcomes.
- Nothing on a client's repository merges without the owner's approval in B7.
