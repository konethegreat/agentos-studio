# Privacy: what stays on your machine

AgentOS Studio has no account, no sign-up, no telemetry and no analytics. Nothing you write, run or
store in Studio is sent to K-ONE IT SOLUTIONS or to the project authors. The only network traffic
Studio itself starts is listed below, and every request has a visible purpose.

## Where your data lives

| Data | Location |
|---|---|
| Vault notes (the shared brain, task queue and memory) | The folder you chose, by default `AgentOS Vault` in Documents |
| Workspaces, permission policies, audit log, goals, missions, update preferences | `studio.db` in the app data folder |
| Saved vault path and preferences | `settings.json` in the app data folder |
| Boot report, boot log and crash log | The app data folder and its `logs/` folder |
| Index files for memory search | `memory-<id>.db` beside `studio.db` |
| Backups taken before Studio updates itself | `backups/` in the app data folder |

The app data folder is `%APPDATA%\agentos-studio` on Windows,
`~/Library/Application Support/agentos-studio` on macOS and `~/.config/agentos-studio` on Linux.
[Troubleshooting](TROUBLESHOOTING.md#read-the-logs-first) lists every file.

Studio also reads and writes files owned by the tools it drives:

- **Hermes home** (`~/.hermes` or the folder Hermes uses on Windows): `config.yaml`, `.env` with
  provider keys, `state.db` with chats, and MCP tokens under `mcp-tokens/`. Studio reads these
  directly and changes configuration through the `hermes` command, so Hermes's own validation runs.
- **Agent registrations:** `~/.claude.json`, `~/.qoder/settings.json` and
  `~/.gemini/config/mcp_config.json` get an `agentos-hub` entry. Existing files are backed up once
  before the first merge.
- **`gh` login:** the GitHub view uses the GitHub CLI's own stored credentials. Studio never copies
  the token.

## Network requests Studio makes

| When | Where | Why |
|---|---|---|
| 20 seconds after start, then every 6 hours | `api.github.com` | Read release lists for AgentOS Studio and Hermes Agent to show an update notice |
| Same check | `registry.npmjs.org` | Read the latest Claude Code version |
| Same check | `raw.githubusercontent.com` | Read the Claude Code changelog |
| When you press Update | `github.com` release assets | Download the signed update and its manifest |
| Whenever an OpenRouter key is stored | `openrouter.ai` credits endpoint | Show the remaining credit balance in Usage |
| Periodically for offline awareness | A TCP connect to `1.1.1.1:443` or `api.openrouter.ai:443` | Decide whether the network is reachable. It opens a socket and closes it; no data is sent |
| Continuously for local models | `localhost:11434` | Ask a local Ollama runtime which models it has. Stays on your machine |
| When you open the GitHub view | `api.github.com` through `gh` | The pull requests, issues and runs you asked to see, with your own GitHub login |
| When you connect and publish to Notion | Notion's servers through the Notion MCP server | The pages you choose to publish, with your own Notion token |
| When you press an install or sign-in fix | The vendor's download or login page | Install a tool or sign in with your own account |

Update checks carry no identifier beyond what any web request carries, such as your IP address, and
they run even if you never connect any other service. They are the reason a new version can be
announced without polling you for data.

## Network requests your agents make

Hermes, Claude Code, Qoder and Antigravity are separate programs with their own vendor privacy
terms. When you chat or run a mission, the prompt and the repository content it includes go to the
model provider you selected: Anthropic, OpenRouter, Google, OpenAI, DeepSeek, Nous or a local Ollama
model. Studio hands the text to the agent and cannot see or intercept the connection the agent makes
to its provider. Choosing Ollama keeps model traffic on your machine.

The same applies to MCP servers you register: they receive the inputs an agent sends them. Notion
receives only what you publish, and GitHub receives only what the GitHub view and your pushes ask
for.

## Keys and secrets

- Keys are stored where the owning tool expects them: Hermes's `.env`, provider environment
  variables, or credentials managed by `gh`.
- After a key is saved, Studio shows its name, never its value.
- Studio does not write key values to `studio.db`, the vault, Notion or its logs.
- Provider and MCP traffic uses the account you signed in with. There is no shared relay and no
  server operated by the project authors.

## Nothing is reported home

There is no crash reporting service, no usage counter and no feature flag service. Crash logs stay
in the app data folder until you attach them to an issue yourself. The boot report is written to
disk and shown in Diagnostics; it does not leave the machine.

The project's issue tracker sees only what you post there. If you attach logs, read them first: they
contain folder paths, which may matter to you even though they contain no key values.

## Deleting your data

- **Vault:** delete or move the folder yourself. Studio never deletes notes.
- **Studio state:** quit Studio, then remove the app data folder. This removes workspaces, policies,
  goals, mission history and logs. Rename it instead of deleting it if you may want it back.
- **Hermes, Claude Code, Qoder, Antigravity and `gh`:** their own uninstall or logout flows own their
  data. Studio does not remove another tool's files.

Uninstalling Studio leaves the app data folder and the vault in place on purpose. That is where your
work is.
