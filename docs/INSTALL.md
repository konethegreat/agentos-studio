# Install AgentOS Studio

Builds are not code-signed with a paid certificate. That is a deliberate choice by the owner,
not an oversight: a certificate costs money every year, and paying it would not make the code
safer. The honest consequence is that Windows SmartScreen and macOS Gatekeeper will ask you to
confirm the first launch. This page says exactly what you will see and what to click, how to
check that the file you downloaded is the file we published, and what to do if Smart App Control
is on.

Nothing here asks you to turn off Windows Defender, add an antivirus exclusion, or lower a
security setting.

## Windows 10 and 11

Download one of these from the [latest release](https://github.com/konethegreat/agentos-studio/releases/latest):

| File | Use it when |
|---|---|
| `AgentOS.Studio_<version>_x64-setup.exe` | Normal install for the current user. This is the one to pick. |
| `AgentOS.Studio_<version>_x64_en-US.msi` | You deploy software with Group Policy, Intune or another Windows Installer tool. |

The NSIS setup installs for the current user and does not need administrator rights. The MSI
follows the usual Windows Installer flow and may ask for administrator rights. Windows 10 and 11
include Edge WebView2; if it is missing, the installer downloads it from Microsoft.

### The SmartScreen prompt

The first time you run the setup, Windows shows a blue dialog titled **Windows protected your PC**
with the text "Microsoft Defender SmartScreen prevented an unrecognized app from starting".

1. Click **More info**. The dialog expands and shows the app name and "Unknown publisher", which
   is expected for an unsigned build.
2. Click **Run anyway**, the button that appeared at the bottom right.

That is the whole bypass. It happens once per downloaded file. The installer then runs normally.
If the dialog has no **More info** link, the block may come from Smart App Control or an
organisation policy instead; see the next section.

### Smart App Control

Windows 11 has a stricter feature than SmartScreen called **Smart App Control**. Check it in
Windows Security, **App and browser control**, **Smart App Control**. It has three states:

- **Off:** the installer behaves as described above.
- **Evaluation:** Windows is still deciding. The installer may run, or it may be blocked with no
  **Run anyway** button.
- **On:** unsigned apps that Microsoft has not seen are blocked, and there is no per-app override.
  Turning Smart App Control off is permanent: Microsoft does not let a device turn it back on
  without reinstalling Windows.

This release has no signed installer to offer, so on a machine where Smart App Control is On or
refuses the file, the honest options are:

- install on a machine where Smart App Control is Off, or
- try it in a Windows virtual machine, or
- build from source, which produces a local binary that Smart App Control does not block the
  same way. The steps are in the [README](../README.md#build-from-source).

We do not recommend turning off a Windows security feature just to install one app. If you decide
to do it anyway, that is your machine and your call, and the setting cannot be restored without a
Windows reinstall.

### Check the download

Every release attaches one checksum file per platform:
`SHA256SUMS-windows-x64.txt`, `SHA256SUMS-macos-arm64.txt`, `SHA256SUMS-macos-x64.txt` and
`SHA256SUMS-linux-x64.txt`. Each line is the SHA-256 hash of one installer.

In PowerShell, in the folder where you saved the installer:

```powershell
Get-FileHash ".\AgentOS.Studio_0.13.0_x64-setup.exe" -Algorithm SHA256
```

Open the matching checksum file (or run `Get-Content .\SHA256SUMS-windows-x64.txt`) and compare
the hash for the same file name. The two strings must be identical, character for character.

If they differ, do not run the file. Delete it, download it again from the release page, and
check again. If it still differs, open an issue on the repository with the file name and both
hashes; do not run the installer.

## macOS

Download the DMG for your Mac:

| File | Machine |
|---|---|
| `AgentOS.Studio_<version>_aarch64.dmg` | Apple Silicon (M1 and later) |
| `AgentOS.Studio_<version>_x64.dmg` | Intel |

The build is not notarized, so Gatekeeper stops the first launch with "Apple could not verify
AgentOS Studio is free of malware". Two ways past it, both from Apple:

- Control-click (or right-click) the app in Applications, choose **Open**, then **Open** again in
  the dialog, or
- try to open it once, then go to System Settings, **Privacy and Security**, scroll to
  **Security**, and click **Open Anyway**, then confirm.

Apple's own instructions are in
[support.apple.com/en-us/102445](https://support.apple.com/en-us/102445). Do not disable Gatekeeper
system-wide.

Check the download with:

```bash
shasum -a 256 ~/Downloads/AgentOS.Studio_0.13.0_aarch64.dmg
```

Compare with the matching line in `SHA256SUMS-macos-arm64.txt` (or
`SHA256SUMS-macos-x64.txt` for Intel). The same rule applies: if it differs, download again and
do not run the DMG.

## Linux

| File | Install |
|---|---|
| `AgentOS.Studio_<version>_amd64.AppImage` | `chmod +x` the file, then run it |
| `AgentOS.Studio_<version>_amd64.deb` | `sudo apt install ./AgentOS.Studio_<version>_amd64.deb` |
| `AgentOS.Studio-<version>-1.x86_64.rpm` | `sudo rpm -i AgentOS.Studio-<version>-1.x86_64.rpm` or `sudo dnf install` |

The app needs WebKitGTK 4.1 and GTK 3. The deb and rpm declare them as dependencies. If an
AppImage refuses to start because FUSE is missing, install the FUSE 2 package for your
distribution (for example `libfuse2` on Ubuntu 22.04) or run it with
`--appimage-extract-and-run`.

Check the download with:

```bash
sha256sum AgentOS.Studio_0.13.0_amd64.AppImage
```

Compare with the matching line in `SHA256SUMS-linux-x64.txt`.

## After the install

1. Start AgentOS Studio.
2. Open **Setup** (System group in the rail) and press **Auto-Setup and Repair Everything**.
   Missing tools are installed with winget, Homebrew or the Hermes installer, and sign-in windows
   open where one is needed. Rows marked **optional** are skipped; set those up one by one.
3. On a machine with no vault yet, the first launch already creates one in Documents and saves
   it. You can pick a different folder in Setup at any time.

A machine with only Hermes is a supported setup. Claude Code, Qoder, Antigravity and Notion are
optional rows and never count against the health of the setup.

## Updates after the first install

Studio updates itself through the updater built into the app. Every update bundle is signed with
the project's Tauri updater key and the signature is checked before the update is installed. That
key protects updates from a tampered download; it is not a Windows publisher certificate, and it
does not remove the first-install SmartScreen prompt.

## Related pages

- [Getting started](GETTING-STARTED.md) for the first ten minutes.
- [README](../README.md#build-from-source) to build from source.
