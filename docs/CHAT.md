# Chat

The Chat view talks to Hermes or Claude Code with every MCP server you have connected. Hermes chats
are real Hermes sessions: they appear in `hermes sessions list` and resume with `hermes --resume`.

## Folder

A chat runs in a folder. New chats start in the last folder you used, then the active project's
root, then your home folder; system folders are refused. Pick a repository in the folder picker
before the first message so the agent works inside it and the chat is filed under that project.

## Context and compaction

The usage strip above the conversation shows the model, how full the context window is, what the
session has cost and how many messages Hermes has compacted. A figure with a `~` is estimated from
text length, because Hermes does not record per-message token counts.

When the window fills, Hermes compacts older turns into a summary, and detail is lost. Before that
happens, either fork at a recent reply or start a new chat for the next topic.

## Forking at a message

Hover any message in a Hermes chat and press the fork icon.

- At a reply: the new session holds the conversation up to and including that reply.
- At one of your messages: it holds everything before it, and your message goes back into the
  composer so you can edit and resend it.
- At the very first message: there is nothing to carry, so a new chat starts with that message as
  the draft, recorded as a branch of the original.

The fork is created by `hermes sessions import`, read back to check every turn landed, titled
"Fork of ...", and linked to its parent as a branch. It carries the text of each turn plus a
`[ran tool: name]` line per tool call. Tool outputs, reasoning and the system prompt are not copied.
The original chat is never changed.

## Approvals, Stop and reasoning (ACP)

Hermes chats run over the Agent Client Protocol by default (Providers > Hermes > Chat transport):

- **Approvals.** When Hermes flags a command as dangerous and its own policy does not settle it
  (`approvals.mode`: `smart` lets Hermes' guardian approve low-risk commands itself, `manual`
  always asks), Studio shows the command with Hermes' choices: allow once, allow for the session,
  allow always, deny, deny always. Nothing runs until you choose; closing the dialog denies it.
  An approval nobody answers is denied after a timeout (default 10 minutes, Providers > Hermes >
  Chat transport); the chat says "Denied after timeout" and Windows shows a notification. A timeout
  always answers "deny" for this one command, never "deny always".
- **Stop** cancels the turn in Hermes (`session/cancel`); the reply so far stays. If Hermes has not
  stopped within 20 seconds, Studio drops the connection.
- **Thinking.** Reasoning streams into the collapsible Thinking panel above each reply, when the
  model produces it.

How it works: a new chat is created with interactive `hermes chat` and its creation flags (model,
skills, `--checkpoints`), which stores the session before any turn; Studio reads the id and ends
that process. Every turn then runs on one long-lived `hermes acp` connection, started when the
Chat view opens. A warm turn reaches its first token in about 3 seconds; the first turn of a new
chat also pays for creating the session (about 7 seconds).

Falls back to `hermes chat -q` for one turn when ACP cannot create or load the session (before the
message is sent, so nothing is lost), for slash commands, and for a skill added mid-chat (sent as
`/<skill>`, which ACP would pass to the model as text). With the transport set to
`hermes chat -q`, every dangerous command is denied (`approvals.single_query_mode`) and Stop ends
the process. Tool cards over ACP carry Hermes' title (`terminal: rm -rf x`) rather than structured
input.

## Continue in terminal

Continue in terminal opens the same conversation with `hermes --resume` (or `claude --resume`) in
the terminal panel. Reasoning streams live there and interactive prompts can be answered directly.
The chat in this view keeps working too.

## Plan first

**Plan first** (Hermes chats, above the composer) makes the next message a planning turn. Studio
sends it as Hermes' `/plan`, limited to the chat's folder: Hermes reads what it needs and writes a
Markdown plan to `.hermes/plans/` in that folder, and changes nothing else. The plan appears above
the composer:

- **Approve and run** sends one turn that implements that plan file step by step.
- **Revise** puts "Revise the plan:" in the composer; your feedback produces a new plan.
- **Dismiss** keeps the plan file and does nothing.

The toggle stays on until you turn it off, so every request goes through a plan. Messages that start
with `/` are never wrapped.

## Undo last turn (checkpoints)

New Hermes chats are created with `--checkpoints`: before a tool writes or deletes files, Hermes
snapshots the folder in its own checkpoint store. **Undo last turn** opens the Hermes TUI on this
chat in the terminal panel with `/rollback` typed in. Press Enter to list the checkpoints, then
`/rollback <number>` to restore one; Hermes restores the files and rewinds the conversation to that
point. The TUI is used because Hermes serves `/rollback` there and refuses it for single-query
turns. Chats created before checkpoints were on have nothing to restore.

## Skills

The Skills button preloads skills when a chat is created (`-s <skill>`), so the agent starts with
that knowledge loaded rather than finding it by chance. Hermes fixes a session's skills at creation
and refuses a resume that changes them, so a skill picked after the first message is invoked on the
next message as `/<skill> <your message>`, one skill per message.
