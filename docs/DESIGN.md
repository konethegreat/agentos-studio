# Design tokens and themes

Studio has a dark theme and a light theme. The status bar button cycles Follow Windows, Dark and
Light. The choice is stored in `localStorage` (`studio-theme`) and applied by `public/theme-init.js`
before the first paint, so a light-theme user never sees a dark flash. It is a file, not an inline
script, because the app CSP only allows scripts from `'self'`.

All tokens live in `src/index.css`. Dark is the reference; `:root[data-theme="light"]` overrides.

## Semantic tokens

Components ask for a role, never a hex value.

| Token | Role | Utility examples |
|---|---|---|
| `sunken` | deepest surface: rail, terminal panel, mission strip | `bg-sunken` |
| `inset` | status bar, code blocks, chat sidebar | `bg-inset` |
| `canvas` | page background | `bg-canvas` |
| `panel` | drawers, onboarding, board columns | `bg-panel` |
| `card` | cards, menus, dialogs | `bg-card` |
| `raised` / `raised-hover` | secondary buttons and their hover | `bg-raised`, `hover:bg-raised-hover` |
| `line` | borders and dividers | `border-line`, `ring-line` |
| `fg`, `fg-strong`, `fg-muted`, `muted` | text roles | `text-fg-muted` |
| `accent` | brand green: focus ring, selection, checkboxes | `outline-accent` |
| `ok`, `danger`, `warn`, `info` | status icons and counts | `text-ok`, `text-danger/80` |
| `knob` | toggle knob | `bg-knob` |

Type scale, by role: `text-micro` 9.5px, `text-2xs` 10px, `text-caption` 10.5px, `text-label`
11px, `text-small` 11.5px, `text-ui` 12px, `text-body-sm` 12.5px, `text-body` 13px, `text-chat`
13.5px, `text-title` 15px. Radius uses Tailwind's scale (`rounded-md`, `rounded-lg`, `rounded-xl`).
Motion uses `--duration-fast`, `--duration-base` and `--ease-standard`.

## How the light theme works

- The semantic tokens above get light values.
- The neutral ramp is theme aware. `text-slate-100` is the strongest text in dark and the darkest
  text in light, and so on down the ramp. Each light value was chosen to give the same contrast on
  the light canvas that the dark value gives on the dark canvas, so hierarchy is preserved.
- `white` becomes near-black in light, so translucent overlays such as `bg-white/5` turn into
  subtle dark tints instead of vanishing.
- Pale accent text (the 100 to 300 shades of emerald, sky, amber, red, violet and a few others)
  maps to the 700 to 900 shades in light, so `text-emerald-300` stays readable on white (about
  5:1). Solid accent fills (`bg-emerald-500`) are the same in both themes.
- Code blocks and the terminal follow the theme with their own light palettes.

## Accessibility

- Keyboard focus draws a 2px accent outline on buttons, links, summaries and anything focusable;
  mouse clicks do not.
- `prefers-reduced-motion: reduce` turns animations and transitions off, spinners included.

## Rules for new code

- Use the semantic tokens and the type scale. Do not add hex values or `text-[Npx]` sizes in
  components; `npm run build` will not stop you, review should.
- A new surface that is not covered by a token gets a token in `src/index.css` with both a dark
  and a light value, in the same change.
- Check both themes before merging UI work.

## How this was verified

Every screen was captured in both themes from a dev build with a stubbed Tauri bridge. Each dark
capture was pixel-diffed against the same screen on `main`. The only differences were the new theme
button, transient states (spinners, toasts, hover), and the board columns, which moved by one RGB
unit when two near-identical panel shades were merged into one `panel` token.
