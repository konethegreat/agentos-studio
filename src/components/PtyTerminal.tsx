import { FitAddon } from "@xterm/addon-fit";
import { Terminal } from "@xterm/xterm";
import "@xterm/xterm/css/xterm.css";
import { useEffect, useRef } from "react";
import { studio, studioEvents } from "../lib/studio";
import { currentTheme, onThemeChange, type Theme } from "../lib/theme";

const THEME = {
  background: "#080b11",
  foreground: "#d8dee9",
  cursor: "#34d399",
  cursorAccent: "#080b11",
  selectionBackground: "#10b98155",
  black: "#1f2937",
  red: "#f87171",
  green: "#34d399",
  yellow: "#fbbf24",
  blue: "#60a5fa",
  magenta: "#c084fc",
  cyan: "#22d3ee",
  white: "#e5e7eb",
  brightBlack: "#4b5563",
  brightRed: "#fca5a5",
  brightGreen: "#6ee7b7",
  brightYellow: "#fde68a",
  brightBlue: "#93c5fd",
  brightMagenta: "#d8b4fe",
  brightCyan: "#67e8f9",
  brightWhite: "#f9fafb",
};

/** The same palette tuned for a light background, so the terminal follows the app theme. */
const LIGHT_THEME = {
  // Matches the light "sunken" surface the terminal panel sits on.
  background: "#eceff4",
  foreground: "#1f2328",
  cursor: "#047857",
  cursorAccent: "#eceff4",
  selectionBackground: "#10b98140",
  black: "#24292f",
  red: "#cf222e",
  green: "#116329",
  yellow: "#7d4e00",
  blue: "#0969da",
  magenta: "#8250df",
  cyan: "#1b7c83",
  white: "#6e7781",
  brightBlack: "#57606a",
  brightRed: "#a40e26",
  brightGreen: "#1a7f37",
  brightYellow: "#633c01",
  brightBlue: "#218bff",
  brightMagenta: "#a475f9",
  brightCyan: "#3192aa",
  brightWhite: "#8c959f",
};

const themeFor = (t: Theme) => (t === "light" ? LIGHT_THEME : THEME);

/** A real terminal bound to a Rust PTY. Stays mounted while hidden so no output is lost. */
export function PtyTerminal({ id, active, exited }: { id: string; active: boolean; exited: boolean }) {
  const host = useRef<HTMLDivElement>(null);
  const term = useRef<Terminal | null>(null);
  const fit = useRef<FitAddon | null>(null);

  useEffect(() => {
    if (!host.current) return;
    const t = new Terminal({
      fontFamily: '"JetBrains Mono", "Cascadia Code", Consolas, monospace',
      fontSize: 12.5,
      lineHeight: 1.2,
      cursorBlink: true,
      scrollback: 10000,
      allowProposedApi: false,
      theme: themeFor(currentTheme()),
    });
    const unTheme = onThemeChange((th) => {
      t.options.theme = themeFor(th);
    });
    const f = new FitAddon();
    t.loadAddon(f);
    t.open(host.current);
    term.current = t;
    fit.current = f;
    let disposed = false;

    // Replay what the terminal printed before this view mounted, then stream. Chunks
    // carry a sequence number so nothing in the snapshot is written twice.
    const buffered: { data: string; seq: number }[] = [];
    let since = -1;
    const unData = studioEvents.ptyData((e) => {
      if (e.id !== id) return;
      if (since >= 0) {
        if (e.seq > since) t.write(e.data);
      } else buffered.push(e);
    });
    void studio.pty
      .snapshot(id)
      .then((s) => {
        if (disposed) return;
        if (s.data) t.write(s.data);
        since = s.seq;
      })
      .catch(() => {
        since = 0;
      })
      .finally(() => {
        buffered.splice(0).forEach((e) => {
          if (e.seq > since) t.write(e.data);
        });
      });

    const onInput = t.onData((d) => void studio.pty.write(id, d).catch(() => undefined));
    const onResize = t.onResize(({ cols, rows }) => void studio.pty.resize(id, cols, rows).catch(() => undefined));
    const ro = new ResizeObserver(() => {
      if (host.current && host.current.offsetParent !== null) {
        try {
          f.fit();
        } catch {
          // the element is hidden or has no size yet
        }
      }
    });
    ro.observe(host.current);
    window.setTimeout(() => {
      try {
        f.fit();
      } catch {
        // the element is hidden or has no size yet
      }
    }, 30);

    return () => {
      disposed = true;
      unTheme();
      ro.disconnect();
      onInput.dispose();
      onResize.dispose();
      void unData.then((u) => u());
      t.dispose();
    };
  }, [id]);

  useEffect(() => {
    if (!active) return;
    const tm = window.setTimeout(() => {
      try {
        fit.current?.fit();
      } catch {
        // hidden
      }
      term.current?.focus();
    }, 20);
    return () => window.clearTimeout(tm);
  }, [active]);

  useEffect(() => {
    if (exited) term.current?.write("\r\n\x1b[90m[process exited]\x1b[0m\r\n");
  }, [exited]);

  return <div ref={host} className={`h-full w-full px-2 py-1 ${active ? "" : "hidden"}`} aria-label="Terminal" />;
}
