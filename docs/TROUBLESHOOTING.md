# Troubleshooting

Studio keeps its own evidence: a report from every launch, an append-only boot log and a crash log.
Read those before changing anything, then use Diagnostics to re-run the same checks with a fix
button next to each one.

## Read the logs first

Everything Studio writes lives in its app data folder:

| OS | Folder |
|---|---|
| Windows | `%APPDATA%\agentos-studio` (normally `C:\Users\<you>\AppData\Roaming\agentos-studio`) |
| macOS | `~/Library/Application Support/agentos-studio` |
| Linux | `~/.config/agentos-studio` |

| File | What it holds |
|---|---|
| `boot-snapshot.json` | The last boot report: app version, start time, elapsed time, `ready`, every step with `id`, `status`, `ms` and `detail`, and `previous_crash` when the run before this one died. |
| `logs/boot.log` | One JSON line per launch, the same data without the labels. The last 400 launches are kept. |
| `logs/crash.log` | Panic messages with the time, the source location and a backtrace. Written by a panic hook, because a windowed app has no console to show one. |
| `settings.json` | The saved vault path and preferences. |
| `studio.db` | Workspaces, permission policies, the audit log, goals and missions, updates you skipped. |
| `backups/` | Copies taken before Studio updates itself. |

A `boot.log` line looks like this:

```json
{"at":"2026-11-09T08:12:44Z","version":"0.13.0","ready":true,"elapsed_ms":1841,
 "summary":{"ok":7,"repaired":1,"warn":1,"failed":0,"skipped":0},
 "steps":[{"id":"dirs","status":"ok","ms":1,"detail":"All present"}]}
```

A `crash.log` block looks like this:

```
=== 2026-11-09T08:12:44+02:00 ===
index out of bounds
at src/main.rs:42:9
<backtrace lines>
```

The three fixed boot steps are `dirs` (app folders), `path` (tool search path) and `store`
(`studio.db`). The steps that usually warn are `settings` and `vault` (vault path), `tools` (the
five binaries Studio drives), `hermes` (its configuration), `mcp` (hub registration) and `identity`
(hub tokens). A step with `repaired` fixed itself; `failed` means it could not and Studio needs you.

## Use Diagnostics

Open **System, then Diagnostics**. It shows the last report per step and a button that goes to the
screen where a fix lives: Setup for installs, sign in and the vault, MCP and Providers for hub
registration. It also has **Re-run checks**, which runs every check again right away, and tabs for
`boot.log` and `crash.log` so you do not have to find the folder.

`ready: true` only means no step failed. Warnings are still worth reading: a missing optional tool
is a warning, and so is a vault path that no longer exists.

## The installer or the app will not start

Windows SmartScreen and macOS Gatekeeper both refuse an unsigned build on first launch, and Smart
App Control can block it with no Run anyway button. [Install](INSTALL.md) has the exact clicks for
each case, the Smart App Control states and how to check the download hash.

If the app starts and the window is blank or white:

- Windows needs the WebView2 runtime. Windows 10 and 11 normally ship with it; if it was removed,
  install the Microsoft Edge WebView2 Runtime from Microsoft's site and relaunch.
- Linux needs `webkit2gtk-4.1` and `libayatana-appindicator3`. The release notes link the package
  names for the common distributions.
- macOS uses the system WebView and needs no extra package.

## A tool is installed but Studio says it is not on PATH

A desktop app does not get the shell's current environment, so a tool installed while Studio was
open can be invisible to it. The `path` boot step repairs this process's search path from the saved
user PATH on every launch.

1. Close Studio completely and open it again. The new process reads the current user PATH.
2. Open a terminal and confirm the tool answers: `hermes --version`, `claude --version`,
   `git --version`, `node --version`, `gh --version` (`where <tool>` on Windows shows which file runs).
3. If the terminal cannot find it either, install it from **Setup** or add its folder to your user
   PATH, then sign out and back in so new processes see it.

A tool that answers `--version` in a terminal but not in Studio after a restart is the case worth
reporting; include the `tools` step detail from Diagnostics.

## The vault step warns

Studio is deliberately careful here.

- **First launch with no vault.** Studio creates `AgentOS Vault` in your Documents folder, writes the
  folder tree and seed notes, and saves the path in settings. No action needed.
- **A saved vault path is missing.** You moved or deleted the folder, or a network drive is offline.
  Studio reports it and does not recreate it, because creating an empty folder at a saved path would
  hide the real problem. Open **Setup**, pick the folder again, or pick a new one.
- **An existing vault is missing folders.** Studio creates only the missing folders and seed files.
  It never edits or deletes notes you wrote.
- **Permission denied.** Documents folders redirected to a work OneDrive account sometimes refuse
  writes. Pick a folder you own, such as one under your home folder, in Setup.

## Hermes configuration or hub registration fails

- The `hermes` step reads Hermes's `config.yaml`. If Hermes is not installed, install it from
  **Setup**; Studio never writes that file directly.
- The `mcp` step checks that `agentos-hub` is registered with the agents you have. Open **MCP and
  Providers**, use the hub update for each agent, and check that the path it registers still exists.
  If you moved or rebuilt Studio, register again from Setup; an old path is the usual cause. A
  source build has a different exe path than an installed release and should be updated with
  `git pull` and `npm run tauri build`, not with the in-app updater.
- The `identity` step provisions hub tokens. An agent without a token can read the hub but not write
  to it, and Agents shows **read only on the hub**. Run the identity repair from the same screen.

## Sign-in or a model fails

The **Connect your subscriptions** card on Setup shows what is connected and what each option
needs.

- Claude: `claude auth status` in a terminal says whether the CLI is signed in. The card's sign in
  button starts the same login.
- OpenRouter and API providers: the key is written to Hermes's own `.env`, where Hermes reads it.
  Studio shows only the key name afterwards. A live test turn on the provider card proves the key
  and model together.
- Antigravity: the card installs the Hermes OAuth plugin and opens its login.

## Notion returns 401 or 403

Authorise Notion again from the pillar or **MCP and Providers**. A Notion integration only sees
pages that were shared with it in Notion, so a page you just created may need to be shared before
Studio can write to it.

## An update fails

- **Signature or download error.** Download the installer from the release page and install it over
  the current copy. Your vault, chats and `studio.db` are outside the install folder and stay put.
- **Rollback.** Update Center keeps a backup taken before the update and can restore it.
- **Studio closes while installing.** That is expected for a Studio update; it reopens when the
  installer finishes.
- **Source builds.** A copy running from `target/release` or `target/debug` is told not to update in
  place. Use the build commands in the README so the hub path registered with your agents does not
  change.

## Studio crashed last time

Open Diagnostics. If the previous launch ended in a panic, the boot report carries
`previous_crash` and the screen says so instead of showing a green startup. The last block in
`crash.log` has the message, the source location and a backtrace.

If the app will not stay open long enough to reach Diagnostics, read `crash.log` directly. The panic
hook writes it before the process exits. Open an issue with that block, `boot-snapshot.json` and the
steps that led to the crash.

## Start over without losing your work

Do not delete the vault. Notes, chats and repositories are not the problem, and the vault may hold
the only copy of your agent memory.

To reset Studio's own state, quit it and rename the app data folder from the table above, for
example `agentos-studio` to `agentos-studio.bak`, then relaunch. Studio starts with default
settings. On the next launch pick your vault in Setup, and the old folder is still there if you need
to restore something from it. Renaming rather than deleting means the single copy of `studio.db`
still exists.

To change only the vault path, do that in Setup; no reset is needed.

## Report a problem

Open an issue at <https://github.com/konethegreat/agentos-studio/issues> with:

- what you did, what you expected and what happened;
- your OS and the Studio version (the boot splash and Updates show it);
- `boot-snapshot.json`;
- the last block of `crash.log` if there was one.

Logs contain folder paths but no key values. Read them before you attach them if a path itself is
sensitive.
