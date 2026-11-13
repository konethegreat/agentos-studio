// Pure helpers for the Explorer: the folder tree, quick-open matching, highlighted lines and diff
// lines. No Tauri imports, so `npm run test:guides` can run them under node.

export interface TreeNode {
  name: string;
  /** Path relative to the root, forward slashes. Folders have no trailing slash. */
  path: string;
  dir: boolean;
  children: TreeNode[];
}

/** Build a sorted tree (folders first, then files, each alphabetical) from flat paths. */
export function buildTree(files: string[]): TreeNode[] {
  const root: TreeNode = { name: "", path: "", dir: true, children: [] };
  const index = new Map<string, TreeNode>([["", root]]);
  for (const file of files) {
    const parts = file.split("/").filter(Boolean);
    let parent = root;
    for (let i = 0; i < parts.length; i++) {
      const path = parts.slice(0, i + 1).join("/");
      const dir = i < parts.length - 1;
      let node = index.get(path);
      if (!node) {
        node = { name: parts[i], path, dir, children: [] };
        index.set(path, node);
        parent.children.push(node);
      }
      parent = node;
    }
  }
  const sort = (n: TreeNode) => {
    n.children.sort((a, b) => (a.dir === b.dir ? a.name.localeCompare(b.name, undefined, { sensitivity: "base" }) : a.dir ? -1 : 1));
    n.children.forEach(sort);
  };
  sort(root);
  return root.children;
}

/** Every folder above a file, outermost first: what must be open to show it. */
export function ancestors(path: string): string[] {
  const parts = path.split("/");
  return parts.slice(0, -1).map((_, i) => parts.slice(0, i + 1).join("/"));
}

/**
 * Quick open: score a path against a query. Every query character must appear in order. Matches
 * in the file name, at word starts and in runs score higher. Returns -1 for no match.
 */
export function matchScore(path: string, query: string): number {
  const q = query.trim().toLowerCase().replace(/\\/g, "/");
  if (!q) return 0;
  const p = path.toLowerCase();
  const nameStart = p.lastIndexOf("/") + 1;
  // A plain substring is the best kind of match, best of all in the file name.
  const sub = p.indexOf(q);
  if (sub >= 0) return 1000 + (sub >= nameStart ? 500 : 0) - p.length;
  let score = 0;
  let pi = 0;
  let prev = -2;
  for (const ch of q) {
    if (ch === " ") continue;
    const at = p.indexOf(ch, pi);
    if (at < 0) return -1;
    if (at === prev + 1) score += 5;
    if (at === 0 || "/-_.".includes(p[at - 1])) score += 8;
    if (at >= nameStart) score += 3;
    prev = at;
    pi = at + 1;
  }
  return score - p.length / 10;
}

/** The best `limit` paths for a query, best first. */
export function quickOpen(files: string[], query: string, limit = 200): string[] {
  if (!query.trim()) return [];
  return files
    .map((f) => [f, matchScore(f, query)] as const)
    .filter(([, s]) => s >= 0)
    .sort((a, b) => b[1] - a[1] || a[0].localeCompare(b[0]))
    .slice(0, limit)
    .map(([f]) => f);
}

const LANGS: Record<string, string> = {
  rs: "rust",
  ts: "typescript",
  tsx: "typescript",
  mts: "typescript",
  cts: "typescript",
  js: "javascript",
  jsx: "javascript",
  mjs: "javascript",
  cjs: "javascript",
  json: "json",
  jsonc: "json",
  md: "markdown",
  markdown: "markdown",
  py: "python",
  go: "go",
  java: "java",
  kt: "kotlin",
  kts: "kotlin",
  cs: "csharp",
  c: "c",
  h: "c",
  cpp: "cpp",
  cc: "cpp",
  hpp: "cpp",
  rb: "ruby",
  php: "php",
  swift: "swift",
  css: "css",
  scss: "scss",
  less: "less",
  html: "xml",
  htm: "xml",
  xml: "xml",
  svg: "xml",
  astro: "xml",
  vue: "xml",
  yml: "yaml",
  yaml: "yaml",
  toml: "ini",
  ini: "ini",
  cfg: "ini",
  sh: "bash",
  bash: "bash",
  zsh: "bash",
  ps1: "powershell",
  psm1: "powershell",
  sql: "sql",
  graphql: "graphql",
  lua: "lua",
  r: "r",
  pl: "perl",
  diff: "diff",
  patch: "diff",
  dockerfile: "dockerfile",
  makefile: "makefile",
};

/** The highlight.js language for a file, or null for plain text. */
export function languageFor(path: string): string | null {
  const name = path.split("/").pop()?.toLowerCase() ?? "";
  if (name === "dockerfile" || name.startsWith("dockerfile.")) return "dockerfile";
  if (name === "makefile") return "makefile";
  if (name === "cargo.lock") return "ini";
  const dot = name.lastIndexOf(".");
  if (dot < 0) return null;
  return LANGS[name.slice(dot + 1)] ?? null;
}

/**
 * Split highlight.js HTML into one string per source line. A span that crosses a line break is
 * closed at the end of the line and opened again on the next, so each line renders on its own.
 */
export function splitHighlighted(html: string): string[] {
  const lines: string[] = [];
  const open: string[] = [];
  let cur = "";
  const re = /(<span[^>]*>)|(<\/span>)|(\n)|([^<\n]+)|(<)/g;
  let m: RegExpExecArray | null;
  while ((m = re.exec(html))) {
    if (m[1]) {
      open.push(m[1]);
      cur += m[1];
    } else if (m[2]) {
      open.pop();
      cur += m[2];
    } else if (m[3]) {
      cur += "</span>".repeat(open.length);
      lines.push(cur);
      cur = open.join("");
    } else {
      cur += m[4] ?? m[5];
    }
  }
  lines.push(cur);
  return lines;
}

/** Escape text for innerHTML. */
export function escapeHtml(s: string): string {
  return s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
}

export type DiffKind = "add" | "del" | "hunk" | "meta" | "ctx";
export interface DiffLine {
  kind: DiffKind;
  text: string;
  /** Line number in the old file, for deletions and context. */
  old?: number;
  /** Line number in the new file, for additions and context. */
  new?: number;
}

/** Classify the lines of a unified diff and number them from the hunk headers. */
export function parseDiff(diff: string): DiffLine[] {
  const out: DiffLine[] = [];
  let o = 0;
  let n = 0;
  let inHunk = false;
  for (const line of diff.replace(/\n$/, "").split("\n")) {
    const h = /^@@ -(\d+)(?:,\d+)? \+(\d+)(?:,\d+)? @@/.exec(line);
    if (h) {
      o = Number(h[1]);
      n = Number(h[2]);
      inHunk = true;
      out.push({ kind: "hunk", text: line });
    } else if (!inHunk || line.startsWith("diff --git")) {
      // Inside a hunk only a new file header ends it: "--- x" there is a deleted "-- x".
      inHunk = false;
      out.push({ kind: "meta", text: line });
    } else if (line.startsWith("+")) {
      out.push({ kind: "add", text: line.slice(1), new: n++ });
    } else if (line.startsWith("-")) {
      out.push({ kind: "del", text: line.slice(1), old: o++ });
    } else if (line.startsWith("\\")) {
      out.push({ kind: "meta", text: line });
    } else {
      out.push({ kind: "ctx", text: line.slice(1), old: o++, new: n++ });
    }
  }
  return out;
}

/** Added and removed line counts of a diff. */
export function diffStat(lines: DiffLine[]): { add: number; del: number } {
  return {
    add: lines.filter((l) => l.kind === "add").length,
    del: lines.filter((l) => l.kind === "del").length,
  };
}

/** The selected line range as text for a chat prompt, with line numbers. */
export function excerpt(lines: string[], from: number, to: number): string {
  const a = Math.max(1, Math.min(from, to));
  const b = Math.min(lines.length, Math.max(from, to));
  return lines.slice(a - 1, b).join("\n");
}

/** A short label for a checkout: the mission for an agent worktree, else the branch. */
export function checkoutLabel(branch: string, main: boolean): string {
  const m = /^agentos\/m-(\d+)-\d+$/i.exec(branch);
  if (m) return `Mission M-${m[1].padStart(3, "0")} (${branch})`;
  if (main) return `${branch || "detached"} (main checkout)`;
  return branch || "detached";
}

/** Human file size. */
export function fileSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
}
