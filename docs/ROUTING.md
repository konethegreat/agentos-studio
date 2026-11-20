# Team routing policy

A repository can commit the model choices its team agrees on, so nobody has to remember them.

`.agentos/routing.json` in the repository root:

```json
{
  "roles": {
    "implementer": { "provider": "claude-subscription", "model": "claude-opus-5-5" },
    "reviewer":    { "provider": "antigravity-oauth",   "model": "gemini-4-argon-medium" }
  },
  "allow_merge": false
}
```

- `roles` must use real roles: planner, implementer, reviewer, gatekeeper, memory.
- A blank provider or model on a mission is filled from the policy. An explicit choice always wins.
- `allow_merge` is the repository's standing answer to "may a mission merge here at all".
- A malformed file is ignored rather than half applied, and the mission falls back to the active model.

## Tool allowlist for headless workers

Claude Code mission workers run `claude -p --permission-mode acceptEdits` in the mission's worktree,
which allows file edits there and nothing else. Shell commands need an allow rule, because a
headless session has nobody to ask. Every worker gets `mcp__agentos-hub`, `Bash(git *)` and
`Bash(gh pr *)`, which is enough to commit, push and open the pull request. A repository adds what
its own checks need:

```json
{
  "allowed_tools": ["Bash(npm test*)", "Bash(cargo *)"]
}
```

- Each `Bash(...)` rule is also passed as `PowerShell(...)`: Claude Code on Windows runs shell
  commands through either tool, and a rule for only one of them is denied half the time.
- A rule with no pattern (`Bash`, `Bash(*)`, `PowerShell(*)`, `*`) or anything that mentions
  skipping permissions makes the file invalid, so the whole policy is ignored. Studio never passes
  `--dangerously-skip-permissions`.
- The list is read from the main checkout, so a worker cannot widen its own permissions by editing
  the file in its worktree.

## Antigravity workers on Windows

Antigravity mission workers run `agy` with the same allowlist rewritten as `command(regex:...)`
rules plus `mcp(agentos-hub/*)`. `agy` runs `run_command` through Windows PowerShell 5.1, and
headless mode cannot prompt for permission: one auto-denied command ends the whole attempt, so a
cmdlet with no rule costs the run.

- The task prompt tells every Antigravity worker its shell is PowerShell 5.1: one command per
  call, no `&&` or `;` chaining, no PowerShell cmdlets for file inspection (the built-in
  `list_dir`, `grep_search` and `view_file` tools cover that), and the commands it runs are `git`,
  `gh` and the test commands the task names.
- Six read-only cmdlets are always allowed on top of the repository rules: `Get-ChildItem`,
  `Get-Content`, `Select-String`, `Test-Path`, `Get-Location` and `Resolve-Path`. Writing cmdlets
  stay out.
- The rules live in `routing::agy_allow_rules` and `bus::AGY_SHELL_NOTE`; both have unit tests.

## Order of precedence

When a mission is created, its provider and model come from the first of these that applies:

1. **Explicit:** what you picked in the New mission form.
2. **routing.json:** the repository's policy for the mission's role.
3. **Router:** only with "Let the router pick" on (Missions), and only for Hermes missions. The
   router scores each provider and model per role from decided missions: smoothed approval rate
   minus a small cost penalty. Only the role's own Hermes missions decided in the last 60 days
   count. It pins only when the role has at least 5 of them, the best candidate has at least 5,
   and that candidate leads the next one by 5 points; otherwise it declines and says why. A model
   also needs at least three of its own eval notes that scored better than the mission they
   replayed; level or losing replays do not count.
4. **Inherit:** Hermes' active model.

Every new Hermes mission also records what the router would have chosen (or why it declined),
even with "Let the router pick" off; hover a mission's model label to see it. That shadow log is
how to judge the router before letting it act.

The mission list shows where a chosen model came from (`via routing.json`, `via router`) with the
reason on hover. The router never changes the global model, a mission that is already pinned, or
a running mission. Off, it only shows what it would pick.


## Choosing the agent (0.12)

A mission for agent `auto` gets its agent from live quota and measured outcomes when it starts, and
non-urgent work waits for a subscription window to reset instead of switching to metered spend. See
[AGENTS.md](AGENTS.md#quota-aware-assignment).
