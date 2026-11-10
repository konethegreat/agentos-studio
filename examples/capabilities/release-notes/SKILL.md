---
name: release-notes
description: Use when a version is about to be tagged. Drafts release notes from the pull requests merged since the previous tag.
license: MIT
author: K-ONE IT SOLUTIONS, Kone Tshivhinda
---

# Release notes from merged pull requests

## Inputs

- The version being released and the previous tag.
- The list of pull requests merged between them (title, number, body), which the caller provides
  or reads with its own tools. This skill does not run commands.

## Output

One Markdown section:

1. `## vX.Y.Z: <theme in five words or fewer>`.
2. One short paragraph saying what the release is for, in plain words.
3. Groups by area (for example Missions, Memory, Safety), each a list of one line per change
   with the pull request number, written as what the user can now do.
4. A "Fixed" group for bug fixes, and a "Known limits" group for anything shipped but not
   verified live.

## Rules

- No em dashes or en dashes. Use hyphens, colons or periods.
- Never invent a change that is not in the list. Leave out chores that change nothing for users.
- Credit the work to the project, not to an agent.

## Refuse when

- The list of merged pull requests is empty or missing: say so instead of writing notes.
