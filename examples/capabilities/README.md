# Example capability packs

Two packs in Studio's own format (`capability.json` at the root), used to try the trust pipeline
in Brain, Capabilities, Capability packs:

- `release-notes`: an official pack, signed with Studio's key (`k-one-official-1`). It asks only
  for `fs.read`, so the permission sheet grants it as is.
- `community-file-count`: an unsigned community pack with a script. It asks for `shell.exec`,
  which a community pack can never be granted, so it installs with `fs.read` only and the script
  never runs.

Change any file in `release-notes` and check it again: the signature no longer matches the content
and the pack is refused.

The format, the tiers and the checks are described in docs/CAPABILITIES.md.

K-ONE IT SOLUTIONS, Kone Tshivhinda.
