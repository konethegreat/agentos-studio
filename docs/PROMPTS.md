# Prompts: hand work to another agent or app

Studio is not a replacement for Claude Code, Antigravity, Qoder, Cursor or Claude Desktop. Often
the right move is to hand a piece of work to one of them: the next phase of a project, a
session that has to continue in a fresh one, a review, one bug. The quality of that handoff is the
prompt, and the hard part of a good prompt is the context: what is done, what is left, the rules,
the files, the checks. Studio already knows those facts, so Work, Prompts writes the prompt from
them.

## Write one

1. Choose the project, what the prompt is for, and who receives it.
   - **Continue the project:** pick up the next open task or phase.
   - **Hand over a session:** carry a Hermes session's work into a new one. Choose the session;
     Studio reads it read-only from Hermes (its last compaction summary, last request and reply,
     the files it changed, its decisions and references).
   - **Review a change**, **Fix a problem**, or **Anything else**.
2. Optionally say it in your own words ("finish phase 3 and release it").
3. **Draft from the project.** Studio gathers, from the repository and its own records:
   branch, remote and uncommitted files; the last commits; open pull requests (`gh`); missions
   for the repository; `PHASES.md` and the open boxes of `TASKS.md` (`docs/agentos/` first);
   `AGENTS.md`; the vault's project note; files to read first; and the checks that prove the work
   (the `package.json` scripts that exist, `cargo`, `pytest`, `go test`). The draft has fixed
   sections: Role, Goal, the context block, Task, Constraints, Done when, Report back.

The facts sit between `<!-- agentos:context -->` and `<!-- /agentos:context -->`. Edit anything
else freely.

## Keep it current

Every prompt is saved with its project, and every changed body is a new version (Load brings
an old one back). **Refresh context** rewrites only the context block, so a prompt saved last
week goes out with today's commits, tasks and missions, and your own wording around it stays.
Saved prompts are mirrored to the vault at `05_Prompts/<project>/P-<id>.md`, so agents find
them through memory recall.

## Check it before it leaves

- **Check** (rules, no model): a goal, a finish line, constraints, a report format; no secrets;
  no em or en dashes; the size suits the receiver (about 3000 tokens for a chat model, 6000 for
  a coding agent, 8000 for Claude Desktop); and the paths you wrote exist in the project.
- **Sharpen**: one quiet Hermes turn rewrites it so the receiver can finish without questions,
  keeping every fact, path and the context block. The old wording stays as a version.
- **What is unclear?**: a Hermes turn reads the prompt as the receiver would and lists every
  question it would have to ask. This is the cheapest way to find the gap before another agent
  spends an hour guessing.

## Hand it over

- **Copy for another app:** for Claude Desktop, a chat model, or any app you paste into.
- **Write into the project:** `.agentos/handoff/P-<id>-<title>.md` in the repository, where an
  agent opened in that folder reads it.
- **An agent name** (hermes, claude-code, antigravity, qoder): a task on the Agent Bus for that
  agent, with the repository named, picked up like any other task.

Every handoff saves first, blanks anything that looks like a secret, turns dashes into hyphens,
and is recorded with the version that went out (History shows each one).

## What it does not do

- It never sends a prompt anywhere on its own; you press the button.
- It does not read other apps' chats. Sessions come from Hermes; the rest comes from the
  repository and Studio's records.
- Sharpen and What is unclear? use your Hermes default provider (a flat subscription for most
  owners), one turn each, kept out of the chat list.
