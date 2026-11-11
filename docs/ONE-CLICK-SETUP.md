# One-click setup (1.1)

Goal: a new user installs AgentOS Studio, presses one button, answers one admin prompt and the
sign-ins only a person can do, and is working. Everything a program can do, Studio does.

## What "Set up everything" does, in order

1. **Reads the machine.** The same checks as the Setup screen (`bootstrapper::check_full_stack_status`).
   Optional items (Notion, IDEs the user does not have) are never touched by this button.
2. **System tools in one elevated pass.**
   - Windows: every missing winget package (Git, Node.js LTS, GitHub CLI, Claude Code) is
     installed by one PowerShell script started with `-Verb RunAs`, so the user answers one UAC
     prompt instead of one per installer. When winget itself is missing, the same script
     registers or downloads App Installer first. The script writes a log Studio streams into the
     Setup log and a result file Studio reads, so every package reports done or failed.
   - macOS: `brew install` for the missing tools. When Homebrew is missing, its official
     installer opens in Terminal, because it asks for the user's password itself.
   - Linux: one `pkexec` prompt runs `apt-get` or `dnf` for the missing tools.
3. **PATH without a restart.** Installers write the machine and user PATH in the registry, which a
   running process never sees. `procs::search_path` reads both, so the next step finds the new
   `git`, `node` and `npm` immediately.
4. **User-level steps:** Hermes (its official installer), the Claude bridge plugin, the PATH fix,
   the Obsidian vault, the hub self-test and the hub registration in every agent that is present.
5. **Other apps:** the hub is added to Claude Desktop (including the Microsoft Store build),
   Cursor, VS Code, Windsurf and the Codex CLI when they are installed. They get no identity
   token, so they can read the bus and the shared memory but never write as an agent. A config
   that is not plain JSON (VS Code allows comments) is left alone with a message, a backup of
   the previous file is kept as `<config>.agentos-backup`, and every app update moves them to
   the new hub like the four agents. Setup, Other apps connects or disconnects each one.
6. **Sign-ins, last and in order:** GitHub (`gh auth login --web`), Claude (`claude auth login`)
   and the Hermes model picker. Each opens its own window; Setup shows which are still open.

## Why the app installer stays per-user (not "run as administrator")

The owner asked for the installer to run in admin mode so installs can go where they need to.
The need is real; an elevated installer is the wrong place for it:

- An admin installer installs per machine under Program Files, and then every in-app update
  needs an admin prompt. The per-user install updates itself silently today.
- An installer cannot show which of five installs failed or retry one. The Setup screen streams
  every line and keeps a Re-check button.
- Studio launches coding agents. Nothing in Studio should run elevated; only the one install
  script does, and it exits when the installs finish.

So the elevation happens once, inside the app, for exactly the installs that need it.

## Not done by design

- Typing passwords or API keys: Studio opens the provider's own sign-in and stores keys only in
  Hermes' env file, by name.
- Changing Windows security policy (Smart App Control, Defender): the owner's decision.
- Installing anything the user did not ask for: optional components stay one-by-one.
