# Notion

Studio connects to Notion with `NOTION_API_KEY` and `NOTION_PARENT_PAGE` from the Hermes env file
(`%LOCALAPPDATA%\hermes\.env`). The key is read by name and never stored or shown by Studio. Every
control works without a key as a dry run that says what would change and sends nothing.

The vault stays the source of truth for notes; Studio stays the source of truth for missions.

## Publish (out)

**Publish** in Memory writes these pages under the parent page: the decision summary (the last
seven days of `DECISION_LOG.md`), the latest morning or evening digest, task status, missions, the
fleet and goals. Each page is tracked in a sync ledger with a content hash, so publishing
unchanged content makes no request, and a changed page has its content replaced, not appended.
The digest and a finished goal also publish, and refresh the goals board when it exists.

## Inbox (in)

**Inbox** pulls the "Inbox for agents" database (`notion.inbox` or `NOTION_INBOX_DATABASE`) into
`00_Inbox` as notes for an agent to pick up. A note that already exists is skipped.

## Missions board (both ways)

**Missions board** creates a database named "AgentOS missions board" under the parent page on first
use, then keeps it in step:

- **Out:** every mission is a row (ID, goal, repository, acceptance, role, agent, status, verdict,
  PR, spend). A row is updated only when its mission changed.
- **Propose from Notion:** add a row with a Goal, Repo and Acceptance and leave ID empty. On the
  next sync Studio creates a mission (Agent defaults to hermes, Role to implementer; the model
  follows the same order as the New mission form: routing.json, then the router if allowed) and
  writes the mission ID back to the row. A row missing Repo or Acceptance, or naming an Agent
  Studio does not know (hermes, claude-code, qoder, antigravity), gets a note saying what is wrong.
- **Trust:** anyone who can edit the board can propose, and a mission starts agents. So a
  proposal is **blocked, "needs approval in Studio"** unless the Notion user who created the row is
  trusted. In Missions, a blocked proposal has **Approve** (queue it) and **Approve and trust
  author** (queue it, and queue that author's future proposals directly). Trusted ids are stored in
  `notion.trusted_authors`.
- **Cancel from Notion:** set Request to Cancel. A queued or blocked mission becomes failed with
  "Cancelled from Notion". A running or in-review mission is refused with a note, because only
  Studio can end its run.
- **Nothing else comes back.** Status, verdict, PR and merge are Studio's: an edit to those columns
  in Notion is overwritten on the next sync, and a merge still needs the review gate.

While Automations are on, the board syncs every 5 minutes, before the scheduler dispatches, so a
mission proposed in Notion can start in the same pass.

## Goals board (both ways)

**Goals board** in Memory creates a database named "AgentOS goals board" under the parent page on
first use (kv `notion.goals_db`), then keeps it in step:

- **Out:** every goal is a row: ID, the outcome, Repos, Done checks (one `kind: target` per line),
  Autonomy, Budget USD, Deadline, Status, Progress ("2 of 3 missions done"), Spent USD, Status note,
  the open question (Escalation and Question) and the Report once the goal ends. A row is updated
  only when its goal changed (ledger key `notion-goal:<id>`), and the row of a goal deleted in
  Studio is archived.
- **Propose a goal from Notion:** add a row with a Goal, Repos and Done checks and leave ID empty.
  Done checks are one per line: `command: npm test`, `url: https://example.com/health` or
  `file: docs/PROBE.md`, optionally ending in `@repo`. Autonomy, Budget USD and Deadline are
  optional. On the next sync Studio creates the goal through the same path as the Goals form, writes
  the goal ID back and starts the planner. A row missing Repos or Done checks, or with a check
  Studio cannot run, gets a note saying what is wrong.
- **Trust:** the same list as the missions board (`notion.trusted_authors`). A trusted author's
  goal runs at the autonomy it asked for (the repository default when empty). Anyone else's goal is
  created at **Watch**, whatever the row asked for: the plan and every mission wait for a click in
  Goals.
- **Answer an escalation from Notion:** the row shows the goal's oldest open question, its allowed
  answers and the deadline. Set **Decision** to one of the allowed answers. On the next sync Studio
  answers that escalation (answered by `notion`) and the goal loop applies it on its next pass,
  exactly like an answer from Goals or from the phone. The Decision is refused, with a note on the
  row, when the last editor of the row is not a trusted author, the question was already answered
  or expired, or the answer is not one of the allowed answers. A handled Decision is cleared.
- **Nothing else comes back.** Status, checks, missions, merges and spend are Studio's; an edit to
  those columns in Notion is overwritten on the next sync.

Once the board exists it syncs every 5 minutes while Studio is open, with Automations on or off:
a goal is the owner's instruction, the same rule the goal loop follows.

## Publish, do not mirror

The vault and `studio.db` stay the source of truth; Notion gets the pages above and the two
boards, nothing else. Every board column has exactly one owner (`notion_sync::OWNERS`,
`notion_goals::OWNERS`, checked by unit tests):

| Board | Studio owns | Notion owns | Proposal, then Studio |
|---|---|---|---|
| Missions | ID, Status, Verdict, PR, Spent USD, Studio note | Request | Goal, Repo, Acceptance, Role, Agent |
| Goals | ID, Status, Progress, Spent USD, Status note, Escalation, Question, Report, Studio note | Decision | Goal, Repos, Done checks, Autonomy, Budget USD, Deadline |

Studio overwrites its own columns on every sync and never writes a value into a Notion-owned
column; it only clears a Request or Decision once it has acted on it. Proposal columns are read
once, when the row has no ID, and are Studio's from then on.

The sync ledger records both directions with a hash. `out` rows (one per page or board row) make
an unchanged publish free. `in` rows record what Studio accepted from Notion: the goal or mission a
proposal row produced (`notion-goal:G-002`, `notion-row:M-004`), a Decision (`notion-decision:E-1`),
a cancel (`notion-cancel:M-004`) and each inbox note. A proposal row that already produced a goal
or mission is repaired (its ID written back) instead of producing a second one when a write-back
failed. Archiving a board forgets its ledger rows, so a new board starts clean. The counts per
direction are in `notion_status` (`ledger_out`, `ledger_in`).
