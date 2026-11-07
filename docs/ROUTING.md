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
