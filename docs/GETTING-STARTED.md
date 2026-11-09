# AgentOS Studio: getting started in ten minutes

AgentOS Studio is a desktop control plane for coding agents. It runs Hermes and Claude Code chats,
gives every agent the same memory (a folder of Markdown notes, the vault) and can hand them missions
that a second agent reviews before anything merges.

Nothing is shared with anyone: chats, keys and notes stay on your machine. See [Privacy](PRIVACY.md).

This page is the shortest path from a download to a finished piece of work. Times are for a normal
laptop and a decent connection.

| Minute | What you do |
|---|---|
| 0 to 2 | Install and open Studio |
| 2 to 4 | Let Setup install what is missing and create the vault |
| 4 to 6 | Connect one model |
| 6 to 8 | Send your first chat |
| 8 to 10 | Queue one mission, or write one goal |

## 0 to 2: install

Download the installer for your platform from
[Releases](https://github.com/konethegreat/agentos-studio/releases/latest) and run it. The build is
not code-signed with a paid certificate, so Windows SmartScreen asks once: click **More info**, then
**Run anyway**. [Install](INSTALL.md) has the exact clicks for SmartScreen, Smart App Control,
macOS Gatekeeper and Linux packages, and the hash command to check your download.

## 2 to 4: first launch and Setup

The first launch runs nine startup checks and shows them on the boot screen. Three of them do work
you would otherwise do by hand:

- It creates the app folders and `studio.db`.
- It repairs this process's search path from your saved user PATH, so a freshly installed CLI is
  found without a restart.
- If no vault exists, it creates `AgentOS Vault` in Documents, writes the folder tree and seed
  notes, and saves the path so the next launch opens the same brain. Existing notes are never
  touched.

When the window opens, go to **System, then Setup**.

- **Auto-Setup and Repair Everything** installs what is missing and opens a sign-in window wherever
  one is needed. On Windows it installs Git, Node.js, the GitHub CLI and Claude Code through winget,
  and Hermes Agent from its official installer. If winget is missing, install "App Installer" from
  the Microsoft Store first.
- Rows marked **optional** are for agents you do not have. A machine with only Hermes and Claude Code
  is a supported setup: Qoder, Antigravity and Notion stay optional and no pillar turns red for
  them.
- **Vault:** the first-launch vault is already in place. To use an existing Obsidian vault or a
  different folder, pick it and press **Initialize and Use**. Studio adds the folders it needs and
  leaves your notes alone.

If anything ends up red or yellow, [Troubleshooting](TROUBLESHOOTING.md) starts from the same report.

## 4 to 6: connect one model

The **Connect your subscriptions** card on the same screen shows what is connected. One is enough.

| Option | What you need | Cost |
|---|---|---|
| Claude subscription | A Claude Pro or Max account. The sign in button opens the Claude login. | Flat monthly |
| Antigravity | The community `antigravity-oauth` Hermes plugin and an Antigravity login. The card installs the plugin and signs in. | Flat |
| OpenRouter | An API key from openrouter.ai, pasted into the card. | Per token |

Keys are written to Hermes's own `.env`, where Hermes reads them. Studio shows only the key name
afterwards, never the value, and never writes it to the vault, Notion or its logs. A live test turn
on the provider card proves the key and the model together.

## 6 to 8: connect your agents and send a chat

Studio's built-in `agentos-hub` server gives every agent the same task queue and memory. Setup
registers it with Hermes and Claude Code, and with Qoder and Antigravity if you have them. Later,
**MCP and Providers** shows whether each agent runs the current hub, with a one-click update.

Then open **Chat**, pick a folder (a repository you want to work in), and talk to Hermes or Claude
Code. Replies stream in with rendered Markdown and expandable cards for every MCP and tool call.
Hover a message to fork the chat from that point. See [Chat](CHAT.md).

## 8 to 10: one mission, or one goal

A mission is a unit of work with a repository, a goal, acceptance criteria and a budget. It is the
only thing that can authorise a merge; a chat never can.

- **One mission:** open **Work, then Mission Control**, fill the form and press **Queue mission**.
  Starting it sends an agent into a separate worktree, so your main checkout is untouched. When it
  opens a pull request, a reviewer on a different provider family judges it, CI must be green and
  the budget must hold before the review gate lets it merge.
- **One goal:** open **Work, then Goals**, press **New goal**, write the outcome in one sentence,
  pick the repository, add at least one done check (a command, a URL or a file) and leave the level
  at **Ask on risk**. Press **Create goal**. Hermes plans it into missions, the missions run, and
  the done checks run against the merged code at the end. See [Goals](GOALS.md).

Both screens show progress, the reviewer's verdict and the cost. You can stop everything at once
with the fleet pause in Automations; work already running finishes.

## Where to go next

- [Glossary](GLOSSARY.md): goal, mission, handoff, autonomy level, council, ask and the rest.
- [Tips and guides](GUIDES.md): the short tips Studio shows when they apply.
- [Automations](AUTOMATIONS.md): missions, roles, the review gate and the cost model.
- [Agents](AGENTS.md): the registry, handoffs, asks, councils and who gets assigned.
- [Goals](GOALS.md): planning, autonomy levels, escalations and the digest.
- [Chat](CHAT.md), [Routing](ROUTING.md), [Notion](NOTION.md), [Computer Use](COMPUTER-USE.md).
- [Troubleshooting](TROUBLESHOOTING.md): logs, startup checks and common failures.
- [Privacy](PRIVACY.md): every file Studio writes and every request it makes.

## Updates

Studio checks for a new version shortly after start and every six hours while it runs. When one is
available, the status bar shows **Update to x.y.z**: it downloads the update, checks its signature
and installs it. Update Center also lists updates for Hermes and Claude Code, keeps a backup before
every update and can roll one back.

## Something is not working

Open **System, then Diagnostics**: it re-runs the startup checks, shows each result and offers a fix
where one exists. The boot log and crash log are readable there, and
[Troubleshooting](TROUBLESHOOTING.md) explains what they contain.
