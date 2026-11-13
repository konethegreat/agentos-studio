// Typed bindings for the Explorer commands (src-tauri/src/explorer.rs).
import { invoke } from "@tauri-apps/api/core";

export interface ExplorerFileList {
  files: string[];
  truncated: boolean;
  git: boolean;
  branch: string;
}

export interface ExplorerFile {
  path: string;
  size: number;
  binary: boolean;
  truncated: boolean;
  text: string;
}

export interface ExplorerChange {
  path: string;
  code: string;
  kind: "modified" | "added" | "deleted" | "renamed" | "copied" | "untracked" | "conflict";
  from: string | null;
}

export interface ExplorerHit {
  path: string;
  line: number;
  text: string;
}

export interface ExplorerCheckout {
  path: string;
  branch: string;
  main: boolean;
}

export const explorer = {
  checkouts: (root: string) => invoke<ExplorerCheckout[]>("explorer_checkouts", { root }),
  files: (root: string) => invoke<ExplorerFileList>("explorer_files", { root }),
  read: (root: string, path: string) => invoke<ExplorerFile>("explorer_read", { root, path }),
  status: (root: string) => invoke<ExplorerChange[]>("explorer_status", { root }),
  diff: (root: string, path: string) => invoke<string>("explorer_diff", { root, path }),
  search: (root: string, query: string, caseSensitive = false) =>
    invoke<ExplorerHit[]>("explorer_search", { root, query, caseSensitive }),
  openEditor: (root: string, path?: string, line?: number) =>
    invoke<void>("explorer_open_editor", { root, path: path ?? null, line: line ?? null }),
};

/** Open the Explorer on a folder (and optionally a file) from anywhere. App listens for this. */
export function exploreFolder(root: string, path?: string) {
  window.dispatchEvent(new CustomEvent("agentos:explore", { detail: { root, path } }));
}
