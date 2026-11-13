// Run: npm run test:guides  (node --experimental-strip-types --test scripts/*.test.ts)
import assert from "node:assert/strict";
import test from "node:test";
import {
  ancestors,
  buildTree,
  checkoutLabel,
  diffStat,
  excerpt,
  fileSize,
  languageFor,
  matchScore,
  parseDiff,
  quickOpen,
  splitHighlighted,
} from "../src/lib/explorerModel.ts";

test("the tree puts folders first and nests files under them", () => {
  const t = buildTree(["src/b.ts", "README.md", "src/a/x.rs", "src/a.ts", ".gitignore"]);
  assert.deepEqual(
    t.map((n) => [n.name, n.dir]),
    [["src", true], [".gitignore", false], ["README.md", false]],
  );
  const src = t[0];
  assert.deepEqual(
    src.children.map((n) => n.path),
    ["src/a", "src/a.ts", "src/b.ts"],
  );
  assert.equal(src.children[0].children[0].path, "src/a/x.rs");
  assert.deepEqual(ancestors("src/a/x.rs"), ["src", "src/a"]);
  assert.deepEqual(ancestors("README.md"), []);
});

test("quick open prefers the file name and plain substrings", () => {
  const files = ["src/views/Explorer.tsx", "src/lib/explorerModel.ts", "docs/EXPLORER.md", "src/views/Today.tsx"];
  assert.equal(quickOpen(files, "explorer.tsx")[0], "src/views/Explorer.tsx");
  assert.equal(quickOpen(files, "exmod")[0], "src/lib/explorerModel.ts");
  assert.deepEqual(quickOpen(files, "zzz"), []);
  assert.deepEqual(quickOpen(files, "  "), []);
  assert.ok(matchScore("src/views/Today.tsx", "tdy") > 0);
  assert.equal(matchScore("src/a.ts", "q"), -1);
});

test("languages come from the file name", () => {
  assert.equal(languageFor("src-tauri/src/explorer.rs"), "rust");
  assert.equal(languageFor("src/App.tsx"), "typescript");
  assert.equal(languageFor("Dockerfile"), "dockerfile");
  assert.equal(languageFor("public/index.html"), "xml");
  assert.equal(languageFor("LICENSE"), null);
  assert.equal(languageFor("a.unknownext"), null);
});

test("highlighted HTML splits into self-contained lines", () => {
  const html = '<span class="hljs-comment">/* one\ntwo */</span> x\n<span class="a">y</span>';
  const lines = splitHighlighted(html);
  assert.equal(lines.length, 3);
  assert.equal(lines[0], '<span class="hljs-comment">/* one</span>');
  assert.equal(lines[1], '<span class="hljs-comment">two */</span> x');
  assert.equal(lines[2], '<span class="a">y</span>');
  assert.deepEqual(splitHighlighted("a &lt; b\n"), ["a &lt; b", ""]);
});

test("diff lines are classified and numbered from the hunk header", () => {
  const d = [
    "diff --git a/x b/x",
    "index 1..2 100644",
    "--- a/x",
    "+++ b/x",
    "@@ -10,3 +10,3 @@ fn main",
    " keep",
    "-old",
    "+new",
    " tail",
    "",
  ].join("\n");
  const lines = parseDiff(d);
  assert.deepEqual(
    lines.map((l) => l.kind),
    ["meta", "meta", "meta", "meta", "hunk", "ctx", "del", "add", "ctx"],
  );
  assert.equal(lines[5].old, 10);
  assert.equal(lines[6].old, 11);
  assert.equal(lines[7].new, 11);
  assert.equal(lines[8].new, 12);
  assert.deepEqual(diffStat(lines), { add: 1, del: 1 });
  // A line that starts with "--" inside a hunk is a deletion, not a header.
  const tricky = parseDiff("@@ -1 +1 @@\n--x\n--- y\n++z\n");
  assert.deepEqual(tricky.map((l) => l.kind), ["hunk", "del", "del", "add"]);
  assert.equal(tricky[2].text, "-- y");
});

test("labels, excerpts and sizes read well", () => {
  assert.equal(checkoutLabel("agentos/m-62-1794511436", false), "Mission M-062 (agentos/m-62-1794511436)");
  assert.equal(checkoutLabel("main", true), "main (main checkout)");
  assert.equal(checkoutLabel("feature/x", false), "feature/x");
  assert.equal(excerpt(["a", "b", "c", "d"], 3, 2), "b\nc");
  assert.equal(excerpt(["a"], 1, 9), "a");
  assert.equal(fileSize(900), "900 B");
  assert.equal(fileSize(2048), "2.0 KB");
});
