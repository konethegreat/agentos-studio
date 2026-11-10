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

## The skill bridge to Hermes

Hermes can read skills from extra folders (`skills.external_dirs` in its config), read-only, with
its own skills winning when names collide. Studio manages one such folder,
`%LOCALAPPDATA%\hermes\skills-external`, so a Qoder or Claude Code skill can be used from Hermes
without copying it into Hermes' own tree:

1. Open the skill in Brain, Capabilities and press **Bridge to Hermes**. Studio adds a directory
   junction named `<source>--<skill>` that points at the skill's own folder. When a junction is not
   possible (another drive), it copies the folder and checks the copy's hash against the source.
2. The first time, press **Add to Hermes config** in the Skill bridge card. Studio backs up
   `config.yaml` to `%APPDATA%\agentos-studio\backups\hermes-config`, asks for approval (Safety,
   `hermes.config.skills_external`) and runs `hermes config set skills.external_dirs`, keeping any
   folders already listed. If you prefer to edit the config yourself, copy the block instead.

Studio refuses to bridge a skill when:

- Hermes already has a skill with that name (Hermes would use its own anyway);
- another copy with that name is already bridged;
- something inside the skill folder links outside it;
- the skill runs a file under `scripts/` that does not exist.

The Skill bridge card lists every bridged skill with its source, license, author and original
path, and says when one is broken (its source folder is gone), stale (a copy whose source changed)
or unmanaged (a folder Studio did not add). Removing a bridged skill deletes the junction only;
the source is never touched. Studio keeps the provenance in `.agentos-bridge.json` in the bridge
folder, never inside the skill.

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

## Agent packs

An agent pack is a tailored agent: a pack of kind `agent-pack` with an `agent.json` next to its
`capability.json`:

```json
{
  "persona": "PERSONA.md",
  "tools": ["read", "shell", "todo"],
  "disallowed_tools": ["edit", "web"],
  "model_policy": ["repo-routing", "router", "agent-default"],
  "permission_mode": "plan",
  "guardrails": ["Never push, merge or edit files."],
  "skills": [],
  "agents": ["claude-code", "hermes"],
  "unattended": true
}
```

- **Tools** use Studio's words, so one pack works on several agents: `read`, `edit`, `shell`,
  `web`, `browser`, `memory`, `skills`, `todo`, `delegate`, and `mcp:<server>:<tool>`.
- **Model policy** is a preference list, never a model id: `repo-routing` (the repository's
  `.agentos/routing.json` for the mission's role), `router` (Studio's model router, Hermes
  missions only, when it is allowed and confident) and `agent-default` (the provider you chose and
  its active model). The first entry that resolves wins.
- **Permission mode**: `plan` (read and propose), `ask` (no edit is auto-accepted) or `edit`.

### The ceiling, enforced by Studio

The tools a worker gets are the pack's tools, minus:

1. its own `disallowed_tools`;
2. any tool whose capability you did not grant when you installed the pack (`edit` needs
   `fs.write`, `shell` needs `shell.exec`, `web` and `browser` need `net.hosts`, MCP tools need
   `mcp.tools`);
3. any MCP tool your scoped tool policy denies for that agent and repository.

On Claude Code the worker keeps only the repository's allowlist entries for the tools that remain,
and every dropped family (Edit and Write, Bash, WebFetch and so on) is passed to
`--disallowedTools`. On Hermes the remaining tools become `-t` toolsets. Hermes' `file` toolset
both reads and writes, so a pack with `read` but not `edit` gets no file toolset on Hermes. A pack
can never widen what a worker could already do, and it runs unattended only when it says
`"unattended": true` and the unattended profile is enforced (Safety).

### Using one

Install the pack in Brain, Capabilities, then pick it under **Agent pack** in the New mission
form. The Agents view lists every installed pack with its ceiling on each agent, including what
was dropped and why. `examples/capabilities/agents` has five official packs: planner, code
reviewer, security auditor, docs writer and researcher.

## What is not here

There is no hosted marketplace: no accounts, ratings or install telemetry. Packs install from a
folder or a git URL you choose. Arbitrary UI plugins are not supported: a pack that needs a screen
ships data, and Studio renders it with its own components.
