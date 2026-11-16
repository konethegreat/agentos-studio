# Shadow Kone: the owner's judgment, delegated

Beyond phase B1. Status: in progress.

## Why

By 2026-11-16 the fleet had run 97 missions. 75 were approved by a second agent and merged, one
was rejected, and 19 failed. Every one of the 19 failed because an escalation was answered "stop"
or expired into it, and each one cancelled its whole goal. The owner is the slowest part of the
fleet, and most of what he is asked he has already answered the same way several times.

Shadow answers those questions for him, the way he answered them, and gives him a window to
overrule it.

## What Shadow will not do

- **Never answer a budget, destructive or merge-allow-list question.** Those are the owner's
  standing switches (the budget, Hermes approvals, the allow list). A repository he keeps merging
  by hand belongs on the allow list, which he sets himself.
- **Never invent a policy.** A rule exists only because the owner answered the same way three
  times in a row, or because he set a standing answer himself.
- **Never act without a window.** Every answer is announced first, on Today and on the phone, and
  the owner's answer inside the window always wins.
- **Never call a model to decide in v1.** The judge for new kinds of question (B1.6) comes later,
  and will only ever propose reversible answers.

## How it decides

1. **Reason class.** Each escalation gets a class from its kind and question text, by a pure
   function: `provider` (a model or runtime failure), `no_pr` (finished without a pull request),
   `no_heartbeat`, `conflict`, `ci_after_fix`, `second_reject`, `done_check`, `plan_failed`,
   `other`, plus the three Shadow never touches: `budget`, `destructive`, `merge_allow_list`.
2. **Precedents.** Every escalation the owner decided himself (answered from Studio, the phone or
   Notion) is a precedent for its class. Expired questions and Shadow's own answers are not.
3. **Standing answers.** On the Autonomy screen the owner can set an answer for a class up front
   ("always retry provider failures"). A standing answer wins over a learned rule.
4. **Learned rules.** If the owner's three most recent answers in a class are the same, that
   answer is the rule. One different answer breaks the streak, so an overrule retires the rule at
   once, and three new consistent answers bring it back.
5. **The window.** When a rule or standing answer covers a new escalation, Shadow records a plan
   (kv `shadow.plan.<E-id>`: answer, due time, source, reason), shows it on Today and sends one
   line to the phone. When the window closes with no answer from the owner, Shadow answers,
   `answered_by = shadow`, and the goal loop applies it like any other answer. The window is 10
   minutes by default and always closes before the escalation's own deadline.

## Data

No new table. Precedents are `escalations` rows; plans are kv `shadow.plan.<id>`; settings are kv
`shadow.enabled` (default on), `shadow.window_minutes` and `shadow.standing.<class>`. A plan
marker is cleared when its escalation id is handed out again, like `escalation.applied.<id>`.

## Measuring it

The Autonomy screen shows Shadow's record: every escalation it planned, what it answered or would
have answered, what the owner answered when he answered first, and the agreement rate. An
overruled plan counts against it.

## Rollout

1. Rules and standing answers with the window (this release).
2. The judge for questions with no rule: a Claude turn with the constitution and the nearest
   precedents, proposing only reversible answers above a confidence of 0.8.
3. Voice: urgent questions near their deadline as a phone call through the `hermes-phone` plugin,
   which asks the owner before every call.

## Refused alternatives

- **Answer everything with a model.** It would answer budget and destructive questions too, and
  its answers could not be traced to anything the owner did.
- **Learn from expired questions.** An expiry is silence, not a decision; learning "stop" from it
  would teach Shadow to give up exactly where the owner was simply away.
- **Majority vote over all history.** One recent change of mind would take many answers to show.
  The streak of the last three follows the owner's current judgment.
