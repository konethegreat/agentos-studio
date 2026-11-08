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
