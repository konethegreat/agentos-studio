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
6. the repository is on the auto-merge allow list.

Any one failing means the PR waits for a human. That is the whole safety story and it is
deliberately boring.

## Execution

- **Scheduler:** a tick every minute reads due missions and enqueues them, respecting a global
  concurrency cap and a per-repository cap of one running mission.
- **Worker:** each mission runs as a Hermes session in the repository folder with the role prompt,
  the mission acceptance criteria and `project_brief` plus `recall` preloaded. Output is a bus task
  completion with a summary and a PR URL.
- **Review:** a second session, different provider, prompts for a verdict as JSON.
- **Keep awake:** while any mission is running, the app holds a Windows execution-state request so
  the machine does not sleep, and releases it when the queue drains.
- **Memory:** every finished mission calls `remember` with an `episode`, and a decision when the
  mission changed architecture. The ledger records what it read and wrote like any chat turn.

## Notion and Obsidian

- One vault note per mission in `01_Projects/<repo>/Missions/`, with frontmatter status.
- The Notion fleet table shows mission counts and the last verdict per repository.
- The weekly decision digest is the publish path described in `ROADMAP.md` v0.4.2. Nothing is
  written back from Notion except the Inbox database.

## What must exist before a mission can merge anything

Per-tool permissions and an audit log already exist per action; they need to become per MCP tool.
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
