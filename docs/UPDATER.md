# Updater signing

The updater does not use a paid certificate. It uses a minisign key pair generated locally, and it
refuses an update whose signature does not match the public key compiled into the app.

## What exists now

- Private key: `%APPDATA%\agentos-studio\keys\agentos-updater.key` on this machine. It is outside
  the repository and must never be committed. Back it up somewhere safe.
- Public key: baked into `src-tauri/tauri.conf.json` under `plugins.updater.pubkey`.
- GitHub secret `TAURI_SIGNING_PRIVATE_KEY`: set, so CI signs every release.
- Endpoint: `https://github.com/konethegreat/agentos-studio/releases/latest/download/latest.json`,
  which `tauri-action` publishes from the tag.
- The app checks on start; when a newer signed version exists, the status bar offers **Update to x.y.z**,
  which downloads, verifies and installs.

## Doing it by hand, from scratch

1. Generate the pair:
   `npx tauri signer generate --ci -p "" -w <path outside the repo>`
   This writes the private key, its `.pub` file, and prints the public key.
2. Put the printed public key in `tauri.conf.json` at `plugins.updater.pubkey`.
3. Give CI the private key:
   `gh secret set TAURI_SIGNING_PRIVATE_KEY --repo <owner>/<repo> < <private key file>`
   and set `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` to empty when the key has no password.
4. Build with `bundle.createUpdaterArtifacts: true` so the installers get `.sig` files.
5. Publish a release from a tag. `tauri-action` uploads the installers, their signatures and
   `latest.json`.
6. To rotate, generate a new pair, replace the pubkey, re-issue a release, and set the new secret.
   Old installs will refuse updates signed with the old key only after the pubkey changes, so ship
   one release with the old key that contains the new pubkey.

## Why not a certificate

A code signing certificate only changes what Windows shows before the app is trusted: publisher name
and SmartScreen reputation. It does not make updates safer, and it is not needed for the updater.
Without it, the first run shows "unknown publisher", which is a trust decision the user makes once.

Mission probe note: the mission pipeline was exercised against this file. Remove at will.

Mission probe 2: exercised the mission loop. Safe to remove.
