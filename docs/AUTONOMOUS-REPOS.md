# Repositories that keep themselves busy

With dozens of repositories, the owner cannot watch each one. An opted-in repository keeps its
own plan, and Studio keeps one goal in flight for it, so agents always have work: they take the
next task, open a pull request, the reviewer judges it, the gate merges it, and a report line
lands in the repository.

## The plan lives in the repository

| File | Written by | What it holds |
|---|---|---|
| `docs/agentos/PHASES.md` | the planning goal | 3 to 6 phases, each `## Phase N: <title>` with a goal and a `Done when:` line |
| `docs/agentos/TASKS.md` | the planning goal; ticked by every task's pull request | the same headings, each with `- [ ] <task>` boxes, one pull request each |
| `docs/agentos/REPORT.md` | every task's pull request | one line per task: date, task, what changed, how it was verified |

Studio reads them from origin's default branch (`git fetch`, then `git show origin/<branch>:`),
never from the owner's checkout, so a half-finished local branch never confuses it. Because the
agents tick the boxes in the same pull request that does the work, the plan on main is always
the truth.

## One pass, every ten minutes

For each opted-in repository with nothing in flight, the first that applies:

1. **No plan on the default branch:** a goal to write PHASES.md, TASKS.md and REPORT.md from the
   code, README, AGENTS.md, issues and history. Done check: TASKS.md exists.
2. **A phase has open tasks:** a goal to finish the first such phase, its open tasks in order,
   one pull request each, each ticking its box and adding a REPORT.md line. Done check: the
   repository's test command (`npm test`, `cargo test`, `pytest`, `go test`) when it has one.
3. **Every task is ticked:** a goal to review what shipped and write the next phase (at most once
   a day per repository; a `Maintenance` phase when nothing else is needed).

The goal then runs like any other: the planner breaks it into missions, the scheduler starts them
within the machine's worker caps, the reviewer (a different agent from the implementer) judges
each pull request, and the gate merges it when CI is green, the budget is intact and the
repository allows merging. When the goal is done, the next pass starts the next one.

Guards: a machine-wide limit of repositories working at once (3 by default, set on the screen),
one goal per repository, at least 30 minutes between goal starts in one repository, nothing while
the fleet is paused, and nothing at all until the Autonomy switch is on. A goal that fails or is
paused holds its repository until the owner looks; it shows in the bell like any goal.

## Opting in

Work, Autonomy lists every Hermes project whose folder is a git repository. **Opt in** opens a
pull request that adds this to `.agentos/routing.json`, keeping every existing field, and merges
it:

```json
{ "allow_merge": true, "autonomy": { "enabled": true, "level": "full" } }
```

It is made in Studio's own worktree from origin's default branch, so the owner's checkout is
untouched; if the repository's branch protection refuses the merge, the pull request stays open
for the owner. Levels: **Full** (the gate merges; needs `allow_merge`), **Ask on risk** (the owner
is asked before a merge), **Watch** (the owner clicks each step). Full without `allow_merge`
reads as Ask. `autonomy.budget_usd` sets a metered budget per goal (0 for flat subscriptions).

**Pause here** stops one repository on this machine only; the committed opt-in stays, so another
machine running Studio keeps working on it.

## Reports

- In the repository: `docs/agentos/REPORT.md`, one line per merged task.
- In the vault: `01_Projects/Autonomy/<repo>.md`, rewritten every pass: opt-in, level, plan
  progress per phase, and the log (goal started, finished, errors), linking each goal's note.
- Each goal's own note and report under `01_Projects/Goals/`, and the morning digest.

## What it will not do

- Push to main, or merge anything the review and merge gate would refuse.
- Start work in a repository that has not opted in through a committed `routing.json`.
- Run more repositories at once than the limit, or start goals while the fleet is paused.
