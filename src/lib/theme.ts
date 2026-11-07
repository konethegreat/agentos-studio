/**
 * Theme preference: follow the OS, or force dark or light. Stored in localStorage so
 * public/theme-init.js can apply it before React mounts; the two must agree on key and values.
 */
export type ThemePref = "system" | "dark" | "light";
export type Theme = "dark" | "light";

const KEY = "studio-theme";
const query = () => window.matchMedia?.("(prefers-color-scheme: light)");

export function themePref(): ThemePref {
  try {
    const v: unknown = JSON.parse(localStorage.getItem(KEY) ?? '"system"');
    return v === "dark" || v === "light" ? v : "system";
  } catch {
    return "system";
  }
}

export function resolveTheme(pref: ThemePref, systemLight: boolean): Theme {
  if (pref === "system") return systemLight ? "light" : "dark";
  return pref;
}

export const currentTheme = (): Theme => (document.documentElement.dataset.theme === "light" ? "light" : "dark");

function apply(pref: ThemePref) {
  const t = resolveTheme(pref, !!query()?.matches);
  if (document.documentElement.dataset.theme !== t) {
    document.documentElement.dataset.theme = t;
    window.dispatchEvent(new CustomEvent<Theme>("agentos:theme", { detail: t }));
  }
}

export function setThemePref(pref: ThemePref) {
  localStorage.setItem(KEY, JSON.stringify(pref));
  apply(pref);
}

export const NEXT_PREF: Record<ThemePref, ThemePref> = { system: "dark", dark: "light", light: "system" };

/** Follow OS changes while the preference is "system". Call once at startup. */
export function watchSystemTheme() {
  apply(themePref());
  query()?.addEventListener("change", () => {
    if (themePref() === "system") apply("system");
  });
}

/** Run `fn` whenever the resolved theme changes; returns an unsubscribe function. */
export function onThemeChange(fn: (t: Theme) => void): () => void {
  const h = (e: Event) => fn((e as CustomEvent<Theme>).detail);
  window.addEventListener("agentos:theme", h);
  return () => window.removeEventListener("agentos:theme", h);
}
