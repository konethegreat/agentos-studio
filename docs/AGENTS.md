# Agents talking to each other (Phase C)

AgentOS Studio's goal for Phase C is that the owner never copies text between tools. Agents hand
work, context and questions to each other through the `agentos-hub` MCP server, and every exchange
is written to one log (`studio.db` table `agent_events`, schema 9) that the run timeline reads.

## The agent registry

Work > Agents shows one card per agent, and the `agents` hub tool returns the same cards, so an agent
choosing who to hand work to sees what the owner sees.

| Field | Where it comes from |
|---|---|
| How it starts | The headless command Studio uses (`hermes kanban`, `claude -p`, `agy`), or "IDE only" for Qoder |
| Strengths | Your settings (Edit on the card, kv `agent.strengths.<agent>`), else built-in defaults; roles with two or more decided missions at 60 percent approval or better are added as measured strengths |
| Tools | What a mission can require of it: git, gh, tests, cli, notion, kanban, web |
| Cost model | Claude Code, Antigravity and Qoder are flat subscriptions. Hermes follows its active provider: a subscription is flat, `custom` or Ollama is local, anything else is metered |
| Quota | `hermes usage --provider <p> --json`: the tightest rate-limit window decides the headroom; OpenRouter reports its credit balance. Readings are cached for five minutes; "Read quota again" refreshes |
| Measured | Approvals out of decided missions the agent implemented, with the router's smoothed score |
| Health | `ready`, `ide only`, `low quota` (under 10 percent headroom), `read-only on the hub` (no identity token) or `not installed` |

Agents that report no quota (Antigravity, Qoder) show "this agent reports no quota to Studio" and
are treated as having room, because a provider that reports nothing cannot be shown to be out.

## Handoff packets

`handoff(to, mission_id, summary, next_step, files?, questions?)` passes a running mission from the
agent that holds it to another agent. Studio builds the packet itself so the receiver never starts
cold:

- the sender's summary, next step, files and open questions;
- the mission's goal and acceptance criteria;
- the diff so far in the mission's worktree (committed on the branch plus anything not committed),
  clipped to 8,000 characters, with a stat;
- failing checks on the mission's pull request (`gh pr checks`);
- the four best `recall` results for the goal in that repository;
- the project brief.

The packet becomes a new Agent Bus task for the receiver with the same `Repository:`, `Worktree:` and
`Branch:` lines, so a headless receiver is started by the bus watcher in the same worktree, and the
packet is the first thing in its prompt. An IDE agent finds it with `fetch_pending_tasks`. The
mission moves to the receiver (`missions::hand_to`, reason in kv `mission.note.<id>`), the sender's
task is closed as handed off (a Hermes kanban task is completed with `--force`), and the exchange is
logged. Only the agent holding a running mission can hand it off, never to itself or to `human`.
After a handoff the sender stops; it does not call `complete_task`.

Verified by `tests/live_handoff.rs` (sandbox repository, temporary bus).

## Asking another agent

`ask_agent(to, question, context?, mission_id?, timeout?)` runs one short headless turn on the target
agent and returns its final answer to the caller:

| Target | Turn |
|---|---|
| `hermes` | `hermes chat -Q --oneshot --max-turns 12 --query-file <prompt> --in <folder>` |
| `claude-code` | `claude -p --output-format json` with only read tools (`Read`, `Grep`, `Glob`) and the hub's `recall`, `project_brief`, `search_shared_memory` |
| `antigravity` | `agy` with one stream-json line and only `mcp(agentos-hub/*)` allowed |

The target runs in the mission's worktree when `mission_id` is given, else in its repository, else in
Studio's own `asks` folder; never in the home folder. The prompt tells it not to edit, commit or call
`ask_agent`, `handoff`, `claim_task` or `complete_task`. While an agent answers, kv
`ask.busy.<agent>` holds a deadline and that agent's own asks are refused, so two agents cannot ask
each other in a loop (MCP clients do not pass their environment to the hub, so the guard lives in
`studio.db`). Qoder is IDE only and cannot be asked. The timeout defaults to 120 seconds (10 to 600).

Every ask and every answer (or failure) is a row in the exchange log with the duration; a metered
Hermes answer adds its cost to the mission's spend.

Verified by `tests/live_ask.rs`: Claude Code asks Hermes and Hermes asks Claude Code in the sandbox
repository, and the Hermes sessions the asks created are deleted afterwards.
