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

## Continue in terminal

Continue in terminal opens the same conversation with `hermes --resume` (or `claude --resume`) in
the terminal panel. Reasoning streams live there and interactive prompts can be answered directly.
The chat in this view keeps working too.

## Skills

The Skills button preloads skills on every turn of the chat (`-s <skill>`), so the agent starts with
that knowledge loaded rather than finding it by chance.
