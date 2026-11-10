# Agent packs

Five tailored agents in Studio's pack format, each signed with Studio's official key. Install one
from Brain, Capabilities, Capability packs (point it at the folder), then choose it under "Agent
pack" when you create a mission. The host enforces each pack's ceiling: the tools it gets are its
request intersected with what you granted at install, your tool policy and the repository's
allowlist.

| Pack | Tools | Mode | Unattended |
|---|---|---|---|
| planner | read, todo, memory, skills | plan | yes |
| code-reviewer | read, shell (repository allowlist only), todo | plan | yes |
| security-auditor | read, shell (repository allowlist only), todo | plan | yes |
| docs-writer | read, edit (Markdown only, by guardrail), todo, skills | edit | yes |
| researcher | read, web, memory, todo | plan | no |

K-ONE IT SOLUTIONS, Kone Tshivhinda.
