# Connections that stay working

Hermes runs on subscriptions and keys: the Claude subscription (a Hermes plugin that drives the
Claude Code CLI), Antigravity (a plugin that drives the `agy` CLI), and API keys such as
OpenRouter. When one of them stops, every mission on it stops. Studio checks them every five
minutes and says what broke and the one command that fixes it.

## What Studio checks

| Check | Source | Problem it shows |
|---|---|---|
| Signed in | `hermes auth status <provider>` | signed out: sign in again |
| Plugin on | `hermes plugins show <plugin>` | plugin off: Hermes disabled it, enable it |
| Recent failures | `API call failed (attempt n/m)` lines in Hermes `logs/errors.log` (they carry `provider=` and the upstream message) | usage limit (with the reset time the provider gave), sign-in errors, timeouts |
| Fallback chain | `hermes fallback list` | no fallback: one usage limit stops all work |
| Claude CLI login | `claude auth status` JSON, `loggedIn: true` only | signed out |

Studio never reads a token. A failure older than six hours no longer counts, a usage limit
counts until the reset it names (an hour when it names none), and a timeout counts for half an
hour.

## Where it shows

- Setup, "Can Hermes run right now?": one row per provider, the active one and any fallback
  marked, with a fix button.
- The notification bell: a provider in use that cannot run is a "needs you" item.
- Windows: one notification when a provider in use changes into a problem state (signed out,
  usage limit, plugin off, failing), not again until its state changes. The bell's switch turns
  these off.

## Fixes

Each fix opens its own window, because sign-ins and pickers need a person:

| Problem | Command |
|---|---|
| Claude signed out | `claude auth login` |
| Antigravity signed out | `hermes auth add antigravity-oauth` |
| Plugin off | `hermes plugins enable <plugin>` |
| Sign-in errors or timeouts | `hermes auth refresh <provider>` |
| Usage limit, or no fallback | `hermes fallback add` (pick a second provider that is signed in) |

A usage limit on a flat subscription is not a fault Studio can clear: the provider resets it.
The fallback chain is what keeps work moving until then.

## Antigravity's model

Each Gemini model on the Antigravity plan has its own quota. Studio's Antigravity workers,
reviews and asks run on the model chosen on Work, Agents (the Antigravity card), and on
Antigravity's own default when none is chosen. When a model's quota runs out, the worker log
says so ("Individual quota reached ... Resets in 53h"); Studio marks Antigravity limited until
that reset and sends its work to another agent. Picking a model that still has quota on the card
brings it back at once.

## Updates that keep connections working

The Claude and Antigravity providers are Hermes plugins, so Updates also watches them
(`hermes plugins check-updates --json`): a plugin that has an update or needs fixing for the
installed Hermes appears next to Hermes, Claude Code and Studio, and Update runs
`hermes plugins update`. Updates are checked shortly after launch and every three hours; when a
check cannot reach GitHub, npm or the plugin sources, the next check runs in fifteen minutes
instead of waiting.
