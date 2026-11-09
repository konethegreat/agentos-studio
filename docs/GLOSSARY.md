# Glossary

The words Studio and the docs use, in plain language. Each entry is one or two sentences; follow
the link where a page explains the whole mechanism.

**ADR (architecture decision record).** A short Markdown file in `docs/adr/` that states a decision,
the context, the positions taken and what was rejected. A council writes one when a mission is
marked as a design decision.

**Agent.** A coding assistant Studio can run or register: Hermes, Claude Code, Qoder and Antigravity
are the four it knows. Studio is the control plane around them, not an agent itself.

**agentos-hub (the hub).** The MCP server built into the same binary as the app. It runs only when
an agent starts it (`agentos-studio --mcp-stdio --vault <folder>`), speaks JSON-RPC over stdio with
no port, and gives every agent the same task queue, memory, identity, handoffs and asks.

**Approval.** A prompt that asks you to confirm one specific action before it runs. Approvals come
from the permission policy and from destructive operations; doing nothing never runs anything.

**Ask.** One short headless turn on another agent that returns its answer to the asker. The asking
agent keeps the work; see [Agents](AGENTS.md#asking-another-agent).

**Audit log.** The local record of actions Studio allowed, asked about or denied, stored in
`studio.db`. Safety shows it.

**Autonomy level.** How far a goal runs without you: `watch` (approve the plan, start each mission,
confirm each merge), `ask` (Ask on risk, the default: runs on its own, merges on its own only for
repositories on the merge allow list) or `full` (merges whenever the gate is clear). None of them
skips review, CI or the budget; see [Goals](GOALS.md#autonomy-levels).

**Boot report.** What the nine startup checks found on this launch. Diagnostics shows the last
report; `boot-snapshot.json` and `boot.log` keep it on disk. See
[Troubleshooting](TROUBLESHOOTING.md#read-the-logs-first).

**Constitution.** The rule files (AGENTS.md and friends) that Studio broadcasts to every local
repository, so each agent reads the same instructions.

**Council.** A mission marked as a design decision where two or three agents from different provider
families each write a position and one of them synthesises an ADR. No code is written by the
council; see [Agents](AGENTS.md#council-for-design-decisions).

**Definition of done.** The checks that decide a goal is finished: a `command` must exit 0 in the
repository, a `url` must answer 2xx, a `file` must exist. They run after every mission in the goal
is done.

**Digest.** The morning and evening summary of what finished, what was spent and what waits on you.
It is written to the vault, mirrored to Notion and sent to your phone when a messaging target is set.

**Escalation.** A question Studio asks you, used only for the listed exceptions such as a passed
budget or a second rejection. Everything else is reported in the digest instead.

**Fleet.** Every agent and local repository under one vault and one hub. Fleet pause stops new work
starts everywhere; work already running finishes.

**Goal.** Your outcome in one sentence plus its repositories, definition of done, budget, deadline
and autonomy level. Studio plans a goal into missions and reports when the checks pass. See
[Goals](GOALS.md).

**Handoff.** A packet that moves a running mission from one agent to another: the sender's summary,
next step, files and open questions plus the diff, failing checks, the best recall results and the
project brief. The receiver starts warm; see [Agents](AGENTS.md#handoff-packets).

**Hub identity.** A token that lets an agent write to the hub. Without one the agent can read but
not claim, complete or remember, and Agents shows it as read only.

**Kanban.** Hermes's own task board format, used for planning and for the Mission Control board.
Studio reads and writes it through the `hermes kanban` command.

**MCP (Model Context Protocol).** The standard way agents call tools and connect to servers.
`agentos-hub` is one MCP server; Notion and GitHub are others.

**Mission.** A durable unit of work with a repository, goal, acceptance criteria, role and budget,
moving `queued -> running -> review -> done`. A mission is the only thing that can authorise a
merge; a chat never can. See [Automations](AUTOMATIONS.md).

**Model.** The specific model a provider runs, such as a Claude model over your subscription or a
named model on OpenRouter.

**Profile.** A Hermes profile: a separate Hermes configuration and memory. Studio reads the default
profile unless you pick another.

**Project brief.** A short summary of one repository: purpose, stack, commands, open tasks, recent
decisions and known gotchas. The hub returns it so an agent does not rediscover the repository.

**Provider.** Who runs the model: your Claude subscription, OpenRouter, Gemini, the Anthropic or
OpenAI APIs, DeepSeek, Nous or a local Ollama model. Keys live where the owning tool reads them.

**Recall.** Ranked search over the vault's shared memory that returns snippets with their file path
and line, so an agent can open the exact source instead of guessing.

**Review gate.** The rule that a finished mission is judged by an agent from a different provider
family before it can merge, with CI green and the budget intact.

**Role.** A prompt template plus a permission set plus a model preference that a mission runs under.

**Shared memory.** Typed notes in the vault that every agent reads and writes through the hub:
decisions, conventions, gotchas and outcomes.

**Skill.** A reusable procedure stored in the vault. Skills can come from successful missions, so
the fleet gets better at repeated work.

**Vault.** A folder of Markdown notes every agent can read: the shared brain. It holds the task
queue under `02_Agent_Bus`, memory under `03_Memory`, projects under `01_Projects` and the
dashboard. The first launch creates one in Documents when none exists.

**Workspace.** A folder that groups chats in Studio. It is organisational only and is unrelated to
git branches or repositories.

**Worktree.** A separate git checkout of one branch, created per mission so agent work never runs in
your main checkout.
