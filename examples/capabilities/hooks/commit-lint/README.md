# Commit message lint (hook pack)

An official AgentOS Studio hook pack, signed with `k-one-official-1`. It checks the newest commit
message of the worktree it runs in (or the message passed as arguments): a conventional type
prefix, a subject under 72 characters, and no em or en dashes.

It asks for `hooks.exec` and `shell.exec`, so it runs only after you grant both on the permission
sheet and approve the `capability.run` action. Studio runs it with an empty environment apart
from the unattended allowlist and `AGENTOS_PACK_ID`, in the chosen mission worktree or a sandbox,
with a timeout.

K-ONE IT SOLUTIONS, Kone Tshivhinda.
