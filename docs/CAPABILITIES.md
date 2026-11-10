# Capabilities and the trust model

AgentOS Studio keeps one view of what your agents can do, and one way to add more. This page
covers both: the read-only index of what is already on the machine, and capability packs, which
Studio installs only after checking them and asking you.

AgentOS Studio is built by K-ONE IT SOLUTIONS, Kone Tshivhinda.

## What is on this machine

Brain, Capabilities lists every skill, plugin, agent, command and MCP server that Hermes, Qoder and
Claude Code have installed, plus the MCP servers Studio knows how to add. For each one you see:

- where it lives (the exact file), who published it and under which license;
- a content hash you can copy, so two copies can be compared exactly;
- what it asks for, in eight words: `fs.read`, `fs.write`, `net.hosts`, `shell.exec`,
  `mcp.tools`, `secrets.read`, `hooks.exec`, `model.use`;
- compatibility notes: frontmatter fields a runtime does not read, files it refers to that do not
  exist, absolute paths that only work on one machine, hooks.

A skill that exists in more than one store is one duplicate group. The group lists every copy in
the order a runtime should prefer it: Hermes, then Studio's bridge, then Qoder, then Claude Code,
then any extra folder you added. Copies are never merged or hidden, and a copy whose content
differs from the preferred one is marked.

Studio reads these stores and never writes to them. The only file the index writes is its own
hash cache, `%APPDATA%\agentos-studio\catalog-hashes.json`, so the second scan only rereads
folders that changed.

## Capability packs

A pack is a folder with a `capability.json` at its root. Five kinds exist: `skill`, `agent-pack`,
`mcp-bundle`, `hook-pack` and `command-pack`.

### The manifest

```json
{
  "schema_version": 1,
  "id": "solutions.k-one.release-notes",
  "name": "Release notes from merged pull requests",
  "version": "1.0.0",
  "kind": "skill",
  "description": "What it does, in one or two sentences.",
  "license": "MIT",
  "publisher": { "name": "K-ONE IT SOLUTIONS", "key_id": "k-one-official-1", "homepage": "https://..." },
  "entrypoints": ["SKILL.md"],
  "capabilities": ["fs.read"],
  "requires": { "app_version": ">=0.15.0", "os": ["windows"], "runtimes": ["hermes"] },
  "signature": "hex Ed25519 signature"
}
```

The rules are strict on purpose:

- Every field above is required except `signature`. The optional fields are `signature`,
  `source_url`, `replaces`, `deprecated` and `mcp_tools` (MCP tools the pack calls, as
  `server:tool`).
- A field Studio does not know is an error, not something it ignores. The same goes inside
  `publisher` and `requires`.
- `capabilities` takes only the eight words above. Free text is rejected.
- `id` is reverse DNS style, `version` is semver, and every entrypoint must be a relative path
  inside the pack.

### Signatures and trust tiers

The signature is Ed25519 over the canonical manifest (keys sorted, no whitespace) with
`signature` removed and a `content_sha256` field added: the SHA-256 over every other file in the
pack. So a signature covers the files, not just the claims in the manifest.

| Tier | When | What it can be granted |
|---|---|---|
| official | signed with Studio's key, `k-one-official-1` | everything you allow |
| verified | signed with a publisher key you trust | everything you allow except `secrets.read`, unless you allow it; shell, network and hooks start unticked |
| community | unsigned, or signed with a key nobody trusts | never `shell.exec`, `net.hosts`, `secrets.read` or `hooks.exec` |

"What you allow" is your capability ceiling: kv `marketplace.ceiling`, a JSON map from a
capability to `allow` or `deny`. A capability not in the map is allowed, except `secrets.read`,
which is denied until you allow it.

A signature that does not match the content is a refusal. A signature never downgrades a pack to
community quietly.

### What happens when you install

1. **Stage.** Studio copies the folder, or clones the git URL (https or file only) with no stored
   credentials, no prompts and no symlinks, into a private staging folder. `.git` is dropped.
   Symlinks are not copied and are reported.
2. **Check.** The manifest is parsed strictly, the content is hashed, the signature is checked
   and the quarantine scan runs (see below).
3. **Permission sheet.** You see the tier, the publisher, the hash, every capability it asks for
   and what your policy allows, the addresses it mentions, and every scan finding. A capability
   over the ceiling cannot be ticked. `mcp.tools` is checked tool by tool against your scoped tool
   policy (Safety, Tool permissions): a denied tool stays off.
4. **Activate.** The staged copy is hashed again (a copy that changed after the check is refused)
   and moved to `%APPDATA%\agentos-studio\capabilities\<kind>\<id>\<sha256>\`. A `current` file
   points at the active hash, and it is replaced atomically.
5. **Record.** `%APPDATA%\agentos-studio\capabilities.lock.json` records the id, version, hash,
   publisher key, tier, source, time, requested and granted capabilities, and the earlier
   versions. Every grant and every withheld capability goes to the audit log (Safety, Audit).

Installing never runs anything from the pack.

### The quarantine scan

Refuses the pack:

- hidden Unicode: tag characters (U+E0000 to U+E007F), zero-width characters, bidi controls;
- long base64 or hex blobs, which can hide a payload;
- a script (`.sh`, `.py`, `.ps1`, `.js`, `.bat` and similar, or any file starting `#!`) when the
  pack does not ask for `shell.exec`, and hooks without `hooks.exec`;
- an entrypoint outside the pack or missing, and symlinks;
- binary files that are not images, a file over 2 MB, more than 500 files or 20 MB in total;
- anything that looks like a credential.

Listed for consent:

- every outbound address the pack mentions, one per host.

Marked untrusted:

- text that tries to override system rules ("ignore all previous instructions" and the like).
  Studio does not try to clean prose. It marks the pack and caps what a runtime reads from each
  file at 64 KB.

### Updates, rollback and tampering

- **Update.** Installing a newer version of an installed pack shows the sheet again. A capability
  the update adds is marked "new in this version" and needs an explicit tick of agreement. Nothing
  updates silently.
- **Rollback.** Brain, Capabilities, "Roll back to vX" restores the previous hash, its version and
  its grants. The earlier copy is hashed first.
- **Tampering.** The same version with different content, a pinned hash that does not match, or an
  active copy edited on disk is refused with "hash mismatch". The installed list shows the reason,
  and anything that runs from a pack verifies it against the lockfile first.
- **Remove.** Deletes the lockfile entry and every stored copy of that pack.

### Trying it

`examples/capabilities` in the repository has two packs:

- `release-notes`: official and signed. It asks only for `fs.read`.
- `community-file-count`: unsigned, with a script. It asks for `shell.exec`, which it can never
  get as a community pack, so it installs with `fs.read` only.

Edit any file in `release-notes` and check it again to see a refusal.

### Signing your own packs

A trusted publisher key is added as a key id, a name and a 32-byte Ed25519 public key in hex
(kv `marketplace.trusted_keys`, set through the `capability_set_trusted_keys` command, which is
audited). Studio's own packs are signed on the owner's machine with
`PACK_DIR=<folder> cargo test --lib marketplace::tests::sign_official_pack -- --ignored`. The
private key never leaves that machine.

## What is not here

There is no hosted marketplace: no accounts, ratings or install telemetry. Packs install from a
folder or a git URL you choose. Arbitrary UI plugins are not supported: a pack that needs a screen
ships data, and Studio renders it with its own components.
