# Computer Use

Work > Computer Use shows what Hermes did on your desktop and lets you switch it on or off.

Hermes owns the capability: its `computer_use` toolset sees the screen and drives the mouse and
keyboard through cua-driver. Studio reads that state and changes it only through Hermes' commands.

- **Toolset:** on or off for Hermes (`hermes tools enable/disable computer_use`). Turning it on asks
  for approval first (Safety > `hermes.computer_use.enable`, default Ask), because it lets an agent
  act on your screen.
- **cua-driver:** the version Hermes installed, whether a newer one exists, and the daemon state
  (Hermes starts the daemon when a turn needs it). Install runs `hermes tools post-setup cua_driver`,
  Update runs `cua-driver update --apply` and Doctor runs `cua-driver doctor`, each in the terminal
  so you see the output.
- **Recent actions:** every `computer_use` result Hermes stored (capture, click, type, wait, window
  lists), newest first, with the session it came from. **Screenshot** opens what Hermes saw. Only
  files named `computer_use_*` inside Hermes' image cache are shown; any other path is refused.

Nothing here runs computer use itself. To have Hermes use the computer, ask in a chat; with
approvals on ACP, Hermes asks before anything it flags as dangerous.
