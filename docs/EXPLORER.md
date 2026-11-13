# Explorer

Code, Explorer is a read-only window into a repository. It shows what the agents see and what they
changed, without leaving Studio. It is deliberately not an editor: there is no language server,
debugger or unsaved buffer. When you want to edit, one click opens the file in VS Code at the line
you are looking at.

## What it shows

- **Files.** The repository's file tree, with `.gitignore` applied (tracked files and new files that
  are not ignored). Folders that hold changes carry an amber dot, and changed files are coloured by
  kind. Ctrl+P opens Go to file, which matches the file name first.
- **Code.** A viewer with line numbers and syntax highlighting for the common languages. Files over
  8,000 lines are shown as plain text, and only the first 512 KB of a file is loaded. Markdown files
  have a Preview.
- **Changes.** The working changes against the last commit, untracked files included. Click a file
  to see its diff with old and new line numbers.
- **Search.** Ctrl+Shift+F searches the whole checkout for literal text (ignored files excluded),
  grouped by file. Click a match to open the file at that line.

## Agent worktrees

Each mission attempt works in its own worktree (`<repo>/.worktrees/<mission>-<attempt>`). When a
repository has worktrees, a second picker lists them by mission, for example
`Mission M-062 (agentos/m-062-1794511436)`. Pick one to read the agent's copy and its Changes tab
to review what it did before the pull request exists. Explorer checks the changes every 10 seconds
while it is open, so an agent's edits appear as it works.

## Handing work on

- **Ask in Chat.** Click a line number to pick it, Shift+click another to pick a range, then
  **Ask about lines 12-20**. Chat opens in the repository with the lines quoted in the prompt, ready
  for your question. With nothing picked, the prompt names the file.
- **VS Code.** Opens the checkout in VS Code with the file at the picked line. It needs VS Code's
  `code` command on PATH (in VS Code: Shell Command: Install 'code' command in PATH).
- **Folder** shows the checkout in the system file manager, and **Path** copies the file's path.

Chat's header has a **Files** button that opens the chat's folder here.

## Safety

Explorer only reads. Every request names a checkout and a path inside it. The checkout must be a
Git repository or worktree under one of Studio's scan roots (Setup), and a path that resolves
outside it (`..`, an absolute path, or a link out of the repository) is refused.
