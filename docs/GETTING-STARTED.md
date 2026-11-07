# AgentOS Studio: getting started

AgentOS Studio is a desktop control plane for coding agents. It runs Hermes and Claude Code chats,
gives every agent the same memory (a folder of Markdown notes, the "vault"), and can hand them
missions that are reviewed by a second agent before anything merges.

Nothing is shared with anyone: chats, keys and notes stay on your machine.

## 1. Run Setup

Open **Setup** (System group in the rail). Every row is a check with a button next to it.

- **Auto-Setup and Repair Everything** installs what is missing and opens a sign-in window wherever one is
  needed. Rows marked *optional* are skipped; set those up one by one if you want them.
- Tools it installs on Windows through winget: Git, Node.js, the GitHub CLI and Claude Code.
  Hermes Agent comes from its official installer. If winget is missing, install
  "App Installer" from the Microsoft Store first.
- **Vault:** pick an empty folder (or an existing Obsidian vault) and press *Initialize and Use*.
  Studio creates the folders it needs and leaves your own notes alone.

## 2. Connect a model

The **Connect your subscriptions** card on the same screen shows what is connected. You need one:

| Option | What you need | Cost |
|---|---|---|
| Claude subscription | A Claude Pro or Max account. *Sign in* opens the Claude login. | Flat monthly |
| Antigravity | The community `antigravity-oauth` Hermes plugin and an Antigravity login. *Install plugin and sign in* does both. | Flat |
| OpenRouter | An API key from openrouter.ai, pasted into the card. | Per token |

Keys are written to Hermes's own `.env` file, where Hermes reads them. Studio shows only the key
name afterwards, never the value.

## 3. Connect your agents to shared memory

Studio's built-in `agentos-hub` server gives every agent the same task queue and memory. Setup
registers it with Hermes and Claude Code, and with Qoder and Antigravity if you have them. Later,
**MCP and Providers** shows whether each agent runs the current hub, with a one-click update.

## 4. Start working

- **Chat:** pick a folder, then talk to Hermes or Claude Code. Hover a message to fork the chat
  from that point. See [Chat](CHAT.md).
- **Missions:** give an agent a repository, a goal and acceptance criteria. A second agent reviews
  the pull request. See [Automations](AUTOMATIONS.md).
- **Tips** appear when they apply to what you are doing. See [Tips and guides](GUIDES.md).

## Updates

Studio checks for a new version when it starts and every six hours while it runs. When one is
available, the status bar shows **Update to x.y.z**: it downloads the update, checks its signature
and installs it. The Updates screen lists updates for Hermes and Claude Code too.

## Something is not working

Open **Diagnostics**: it re-runs the startup checks, shows each result and offers a fix where one
exists. The boot log and crash log are linked there.
