# Goals: one sentence in, finished work out

Phase D of the 2026-11-08 master brief (release 0.14.0). The owner states an outcome; Studio plans
it into missions, runs them on the right agents, reviews, merges, checks the result and reports,
and asks only when a rule says it must.

## What a goal is

A goal is the owner's outcome plus everything needed to know when it is finished. It lives in
`studio.db` (table `goals`, schema 10), is mirrored as a vault note
(`01_Projects/Goals/G-001.md`) and published as a row in the Notion goals database.

| Field | Meaning |
|---|---|
| outcome | The sentence the owner wrote. |
| repos | The Hermes projects the work happens in (one or more slugs). |
| definition of done | Runnable checks: `command` (exit code 0 in the repository after the merges), `url` (must answer 2xx), `file` (must exist in the repository). |
| budget | Metered dollars for the whole goal. Flat subscriptions never count. 0 means no metered spend. |
| deadline | Optional date; past it the digest marks the goal late. It never stops work by itself. |
| autonomy | `watch`, `ask` (Ask on risk, the default) or `full`. |

Status: `planning`, `awaiting` (a plan waits for approval at Watch), `running`, `verifying`, `done`,
`failed`, `paused`.

Every mission a goal creates carries `missions.goal_id`; missions without one are the standalone
missions Studio has had since 0.5 and behave exactly as before.

## Autonomy levels

| Level | Plan | Missions | Merge |
|---|---|---|---|
| Watch | shown for approval | each waits for a Start click | a click |
| Ask on risk | starts at once | start on their own | on its own when the repository is on the merge allow list, otherwise an escalation |
| Full | starts at once | start on their own | on its own whenever the gate is clear, allow list or not |

A repository's default level is kv `autonomy.repo.<slug>`; a new goal takes the strictest default
of its repositories unless the owner picks one. No level ever skips the gate: the reviewer is a
different agent, CI must be green with checks present, the budget must hold. Full only removes the
allow-list condition.

The global Automations switch keeps governing standalone missions. A goal's missions run when the
goal is running and the fleet is not paused, because creating a goal at Ask on risk or Full is the
owner's instruction to run it. Fleet pause is the one control that stops everything.

## The loop

```text
goal -> planner (hermes kanban decompose) -> missions under the goal
mission -> worker -> review (automatic, other provider) -> merge (gate) -> next mission
all missions done -> definition-of-done checks -> report
```

1. **Planner.** Studio writes the goal as a triage task in a private kanban database
   (`HERMES_KANBAN_DB=<studio>/plans/<goal>.db`) and runs `hermes kanban decompose <task> --json`.
   Hermes asks its auxiliary model for a task graph (2 to 6 tasks, each with a self-contained spec
   and dependencies) and writes it there. The private database is the reason this is safe: the
   gateway's dispatcher only scans boards under `kanban/boards/`, so nothing in a plan can start as
   a Hermes worker outside Studio's missions and gate (verified 2026-11-09: two children created in
   22 s, both left alone). Studio reads the graph with `kanban show --json`, creates one mission per
   task (agent `auto`, so `assign.rs` picks the agent from quota and outcomes), records each
   mission's dependencies in kv `mission.after.<id>`, and deletes the private database.
   `decompose` with `fanout: false` is `specify`, so one call covers both.
2. **Dependencies.** The scheduler only starts a goal mission whose dependencies are `done`.
3. **Automatic review.** A mission entering review is judged by a reviewer on a different provider
   family from the implementer (Claude Code and Hermes on the Claude subscription are one family;
   Antigravity is another). The reviewer gets the diff, the CI checks and the `hermes verify`
   result, the acceptance criteria and the repository's conventions from memory. A diff over
   60,000 characters is fitted per file, code before docs, with a note listing every changed file
   and which were clipped (see [AUTOMATIONS.md](AUTOMATIONS.md), Review gate). Before the
   reviewer runs, the constitution gate judges the lines the pull request adds (dashes,
   placeholder tags, hidden Unicode, credentials, docs changed with code under `src/`); any
   finding sends the mission back to its implementer as a reject from `constitution-gate`.
4. **Automatic merge.** When `merge_blockers` is empty, the constitution gate passes on the diff
   as it is at merge time, and the level allows it, Studio squash merges and deletes the branch,
   with the provenance trailers from Phase E.
5. **Self-healing.** A failing check on a mission's pull request is rerun once. Passing on the
   rerun makes it a flaky test: a gotcha note, a digest line, no retry loop. Failing again creates a
   fix mission on the same branch with the failing log attached.
6. **Completion.** When every mission is done, the definition-of-done checks run against the
   merged code. All passing: the goal is done, an outcome note is remembered, the project note is
   refreshed, the Notion row updated and a report sent. A failing check gets one fix mission; a
   second failure escalates.

## Escalations

Studio asks the owner only for these, and puts everything else in the digest:

1. The goal's metered budget is passed.
2. A mission is rejected a second time.
3. A mission is blocked or stuck after its retry.
4. Hermes escalates a destructive approval.
5. A merge is needed outside the allow list (Ask on risk).
6. A definition-of-done check fails twice.

An escalation is a row in `escalations` (schema 10) with a question, the allowed answers and a
deadline. It shows in Studio, in the digest and, when a messaging target is set, on the phone
through `hermes send`. A reply on the phone carries the escalation id and a four-digit code that
only went to the phone, for example `E-3 4821 merge`; Studio reads the reply from the gateway's
session store and only the stored hash of the code is kept. When the deadline passes with no answer
the safe answer is taken (do not merge, stop the mission), the same rule as Hermes approvals:
doing nothing never runs anything.

The Notion goals board (`notion_goals.rs`, see `NOTION.md`) is a third place to answer: the goal's
row shows its oldest open question, and a Decision set by a trusted Notion author answers it
(answered by `notion`) through the same `escalation::answer` path. A goal proposed on that board by
anyone else is created at Watch.

## Obsidian views

Every time the goal note is written, Studio also writes the goal's folder next to it,
`01_Projects/Goals/G-001/`:

- `M-004.md` per mission: frontmatter (`type: mission`, status, repo, agent, reviewer, PR, spend)
  and links back to the goal, to the missions it waits for, to its pull request and to the
  decisions taken on it.
- `E-1.md` per decision: the escalation's question, allowed and safe answers, deadline and how it
  ended (answered from Studio, the phone or Notion, or the safe answer at the deadline).
- `G-001.canvas`: the mission graph in the open JSON Canvas format that Obsidian opens natively.
  The goal sits on the left, missions in columns by dependency, decisions in the last column,
  coloured by status.

The goal note links to all of them (Missions, Decisions, Pull requests, Mission graph). Links use
full vault paths, so two goals' `M-004` notes never resolve to each other. Everything is plain
Markdown and JSON that works without Studio. Files Studio wrote carry
`generated_by: agentos-studio`; when an id is handed out again after a cleanup, generated notes
that no longer belong to the goal are removed and any other file in the folder is left alone.

## Budgets

Three ceilings, each on metered spend only (flat subscriptions never count): a mission's budget
stops that mission, a goal's budget pauses the goal and asks, and the global budget in Mission
Control (per day, week or month, read from what Hermes records for every session) stops all
metered work at once and asks once per period. The digest shows spend against the global budget
and against each active goal's budget.

## Digest and fleet pause

- **Digest**, morning and evening (kv `digest.times`, default `07:30,18:00`): what finished, what
  was spent, what is running and what waits on the owner. Written to `04_Agent_Engine/digests/`,
  published to Notion and sent with `hermes send` when a target is set.
- **Fleet pause**: one control that runs `hermes pause --reason`, holds Studio's scheduler and
  refuses new worker starts. Resume reverses all three. Work already running finishes.

## What it will not do

- No model call to choose a model or an agent; assignment stays deterministic (`assign.rs`).
- No Hermes worker outside a mission: the planner's kanban database is private and deleted.
- No silent merge outside the rules above, and no merge without an approving reviewer from another
  agent.
- No retry loops: one CI rerun, one stuck retry, one fix mission per failing check.

## Build order (one pull request each)

1. Goal object and autonomy levels (schema 10, `goals.rs`, the Goals view, vault note).
2. Planner and dependencies.
3. Automatic review on another provider.
4. Automatic merge and escalations, with the answer path in Studio.
5. Self-healing CI.
6. Goal completion, report and Notion row.
7. Digest and phone delivery.
8. Fleet pause.
9. The live done check (`tests/live/live_phase_d_done.rs`) and the release.
