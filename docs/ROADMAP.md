# AgentOS Studio roadmap

## v0.3.0 (this release)

| Area | Ships |
|---|---|
| Hermes parity | Embedded PTY terminal (xterm.js + portable-pty) with tabs for PowerShell or your shell, interactive Hermes, Hermes TUI and Claude Code. "Continue in terminal" opens the current chat with `hermes --resume <id>` (or `claude --resume <id>`) in the same folder. |
| Updates | Hermes (`hermes update --check` plus release notes), Claude Code (npm registry plus CHANGELOG), Studio (GitHub releases). Checked at start and every 6 hours. In-app banner, OS notification, Update, Skip this version, Remind me later. Backups before every update, rollback from the Update Center. |
| Organisation | Workspaces > folders > sub-folders > chats in `studio.db`. Drag and drop, rename, pin, tags, search, archive. Folders map to directories and auto-collect chats run inside them, link to Hermes projects, and hold file attachments that are passed to the agent. |
| GitHub | Auth status, repos, branches, commits, PRs (diff, checks, review, merge, create), issues (create, comment, close), Actions (runs, failed logs, rerun). "Ask Hermes" hands any PR, issue or failed run to the chat. `/gh` slash command. |
| Safety | Per-action permission policies (allow, ask, deny) enforced in Rust, approval modal, audit log view. |
| Guidance | Command palette (Ctrl+K), tooltips on every new control. |

## v0.4.0

- Memory: vault-first memory browser with wikilinks and frontmatter, and a "memory ledger" on every chat turn listing exactly which notes were read and written (from `tool_use` events of `agentos-hub`, file tools and Notion MCP). Notion two-way sync through a vault sync ledger (`notion_id`, `synced_at`, content hash; last-writer-wins with conflict notes).
- Transparency: run timeline across chats, PTYs, workers and updates; reasoning panel fed from `state.db` `reasoning` columns after each turn (stream-json does not stream reasoning; the PTY does); token and cost panel from `state.db` and `hermes insights`; pause and step-in through `hermes pause` and the PTY.
- Design system: semantic tokens for colour, type scale, spacing, radius and motion; light theme; focus rings and full keyboard navigation audit; reduced-motion support.
- Guidance: first-run onboarding, contextual guides, searchable help center built from the docs folder.

## v0.5.0

- Permission model extended to MCP servers and individual MCP tools (mirrors `hermes tools enable|disable`), HTTP APIs and computer use (`hermes computer-use status|doctor`), with per-workspace overrides.
- Studio secrets in the OS keychain (`keyring`), surfacing `hermes secrets` sources.
- Signed builds and the Tauri updater plugin with signed manifests.
- GitHub notifications inbox and PR review comments inline on the diff.

## Ideas worth considering

- Session forking: branch a chat at any message into a new Hermes session.
- Per-folder agent profiles (provider, model, toolsets) so a repo always opens with the right setup.
- Cost guardrails: a monthly budget that pauses dispatch when exceeded.
- Replayable runs: export a run (prompt, tool calls, results) as a Markdown report in the vault.
