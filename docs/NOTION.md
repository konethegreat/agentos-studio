# Notion

Studio connects to Notion with `NOTION_API_KEY` and `NOTION_PARENT_PAGE` from the Hermes env file
(`%LOCALAPPDATA%\hermes\.env`). The key is read by name and never stored or shown by Studio. Every
control works without a key as a dry run that says what would change and sends nothing.

The vault stays the source of truth for notes; Studio stays the source of truth for missions.

## Publish (out)

**Publish** in Memory writes four pages under the parent page: the decision digest, task status,
missions and the fleet. Each page is tracked in a sync ledger with a content hash, so publishing
unchanged content makes no request, and a changed page has its content replaced, not appended.

## Inbox (in)

**Inbox** pulls the "Inbox for agents" database (`notion.inbox` or `NOTION_INBOX_DATABASE`) into
`00_Inbox` as notes for an agent to pick up. A note that already exists is skipped.

## Missions board (both ways)

**Missions board** creates a database named "AgentOS missions board" under the parent page on first
use, then keeps it in step:

- **Out:** every mission is a row (ID, goal, repository, acceptance, role, agent, status, verdict,
  PR, spend). A row is updated only when its mission changed.
- **Propose from Notion:** add a row with a Goal, Repo and Acceptance and leave ID empty. On the
  next sync Studio queues it as a mission (Agent defaults to hermes, Role to implementer; the model
  follows the same order as the New mission form: routing.json, then the router if allowed) and
  writes the mission ID and "Queued by Studio as M-..." back to the row. A row missing Repo or
  Acceptance gets a note saying what it needs.
- **Cancel from Notion:** set Request to Cancel. A queued or blocked mission becomes failed with
  "Cancelled from Notion". A running or in-review mission is refused with a note, because only
  Studio can end its run.
- **Nothing else comes back.** Status, verdict, PR and merge are Studio's: an edit to those columns
  in Notion is overwritten on the next sync, and a merge still needs the review gate.

While Automations are on, the board syncs every 5 minutes, before the scheduler dispatches, so a
mission proposed in Notion can start in the same pass.
