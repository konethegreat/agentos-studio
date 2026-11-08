# ACP spike: Hermes chat over the Agent Client Protocol

Status: spike done 2026-11-07, verdict **not yet**. Superseded 2026-11-08: ACP **is** the Hermes chat
transport, through the hybrid below (`src-tauri/src/acp_host.rs`, `tests/live_acp_chat.rs`).

## 2026-11-08: the hybrid that closed the gaps

Interactive `hermes chat` with creation flags (`-m`, `--provider`, `-s`, `--checkpoints`, `--in`)
stores the session with that policy and prints `Session: <short id>` in its banner before any
turn. Studio resolves the full id from `state.db`, ends the process, and runs every turn over one
long-lived `hermes acp` connection with `session/load`. So the first turn is not a `chat -q` turn
and is not silently denied, which was the objection to the hybrid below.

Measured live on Hermes v0.23.0 (2bd378ed1b8d), Claude Haiku 5.5 through the Claude subscription:

| Check | Result |
|---|---|
| Model fixed at creation | stored model `claude-haiku-5-5[1m]` after ACP turns |
| Approval with `approvals.mode: manual` | asked (`recursive delete: rm -rf ...`), deny honoured, folder survived |
| Reasoning | 20 thought chunks in the first turn (the spike saw none; Hermes now streams `reasoning.delta` to ACP viewers) |
| Cancel | stop reason `cancelled` |
| Session creation | 6.8 to 7.8 s |
| First turn on a cold connection | 6.2 s setup (start plus load), first token at 11 s |
| Warm turn | first token 2.9 s (chat -q: 6.9 s) |

With `approvals.mode: smart`, Hermes' guardian approved the same deletes itself, so Studio only sees
the questions Hermes escalates. Skills added mid-chat still go through `chat -q`, because ACP passes
`/<skill>` to the model as text. The earlier findings follow.

Principle this follows: Hermes owns execution of Hermes work; Studio owns cross-agent orchestration,
the review and merge policy, the vault and visibility.

## What was built

A Rust ACP client on `agent-client-protocol = "=5.0.0"` (feature `process`, protocol v1) that starts
`hermes acp`, initializes, creates or loads a session in a folder, sets the `model` and `reasoning`
config options, streams one prompt turn, answers `session/request_permission` through a channel
(the same shape the ApprovalProvider modal needs) and cancels a turn with `session/cancel`.

Run it: `cargo test --test live_acp_spike -- --ignored --nocapture --test-threads 1`. Set
`ACP_DEBUG=1` to print every raw session update. It deletes every session it creates.

## Results on Hermes v0.23.0 (2bd378ed1b8d)

| Capability Studio uses today | ACP | Evidence |
|---|---|---|
| Stream reply text | yes | `AgentMessageChunk` |
| Dangerous-command approval | **yes** (chat -q denies it silently) | 5 options: allow once, allow for session, allow always, deny, deny always. "deny" was honoured and the folder survived |
| Stop a turn | **yes** (chat -q can only be killed) | stop reason `cancelled` |
| Resume by id | yes, including `--source gui` sessions made by chat -q | turn 4 recalled the previous reply |
| Skills preload (`--skills`) | **no** | `session/new` forwards only cwd and five boolean env flags; `/skill-name` is not a registry command, so ACP sends it to the model as plain text |
| Model fixed at creation | **no** | only `set_config_option model` after creation, which rebuilds the agent: 4.6 to 8.5 s |
| Checkpoints at creation | **no** | not a `session/new` field |
| `/rollback` | **refused** | "not available on the shared gateway yet"; the parity table lists Ink (`rollback.restore`) as the only surface. `/undo` (conversation rewind) is served over ACP |
| `/reasoning`, `/yolo` | yes | answered by the gateway as per-session settings |
| Source tag | same | ACP sessions are stored with source `local`, and so are `chat -q --source gui` sessions (`created_source` desktop); Studio's session list does not filter on source |
| Tool cards and memory ledger | **partial** | built-in tools arrive as `title: "terminal: rm -rf keep-me"`, `kind`, `status`, text content; `name` and `raw_input` are empty, so there is no structured input and no duration |
| Reasoning stream | **not observed** | zero `AgentThoughtChunk` with Claude Sonnet (reasoning high) and DeepSeek v4.1 flash (high), although Hermes asks the owner for `show_reasoning` |

### Latency (same machine, warm gateway)

| Path | Time to first token |
|---|---|
| `hermes chat -q --format stream-json` | 6.9 s (7.0 s total) |
| ACP, new process, new session, default model | about 10 s (start 4.2 to 5.0 s, `session/new` 1.7 to 3.3 s, prompt to token about 3.5 s) |
| ACP, new process, loaded session | 9.3 s |
| ACP, prompt to first token on a live process | about 3.5 s |

ACP is only faster with one long-lived `hermes acp` process per Studio run; a process per turn is
slower than chat -q.

## Decision

ACP cannot carry Studio chat today without losing skills preload, creation-time model and
checkpoints, and structured tool cards. Step 2 (ACP as the chat transport) is
not started. Re-run the spike when Hermes adds creation policy to `session/new` (skills, model,
checkpoints) or serves `/rollback` on the gateway.

A hybrid was considered and not built: create the session with `chat -q` (creation policy intact),
then run later turns over ACP for approvals and stop. Its first turn would still deny approvals
silently, and it needs a decision from the owner, so it is recorded here rather than shipped.

## Consequences for the rest of the Hermes-native plan

- Approvals in Studio chat stay denied by Hermes (`approvals.single_query_mode`, default deny).
  Documented in `CHAT.md`.
- "Undo last turn" cannot use ACP `/rollback`. Files: open the PTY on the session in the Ink TUI
  and type `/rollback`. Conversation only: `/undo` exists over ACP but is not wired.
