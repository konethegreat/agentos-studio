// Commit message lint, an official AgentOS Studio hook pack.
// K-ONE IT SOLUTIONS, Kone Tshivhinda. MIT licence.
//
// Usage: node hooks/commit-lint.mjs [message]
// Without an argument it reads the newest commit message of the git checkout it runs in.
// Studio runs it through the extension runtime: no credentials in its environment, its working
// directory is the mission worktree (or an empty sandbox), and it is killed at the timeout.
import { execFileSync } from "node:child_process";

const TYPES = ["feat", "fix", "docs", "test", "refactor", "chore", "perf", "ci", "build", "style", "revert"];
const DASHES = /[\u2013\u2014]/;

function message() {
  if (process.argv.length > 2) return process.argv.slice(2).join(" ");
  try {
    return execFileSync("git", ["log", "-1", "--format=%B"], { encoding: "utf8" });
  } catch {
    console.error("commit-lint: not a git checkout with commits, and no message was given");
    process.exit(2);
  }
}

const text = message().replace(/\r\n/g, "\n").trim();
const subject = text.split("\n")[0];
const problems = [];
const m = /^([a-z]+)(\([^)]+\))?!?: \S/.exec(subject);
if (!m || !TYPES.includes(m[1])) problems.push(`subject must start with one of ${TYPES.join(", ")} and ": "`);
if (subject.length > 72) problems.push(`subject is ${subject.length} characters; keep it under 72`);
if (DASHES.test(text)) problems.push("message contains an em or en dash; use a hyphen, colon or period");

console.log(`commit-lint (pack ${process.env.AGENTOS_PACK_ID ?? "unknown"}): ${subject}`);
if (problems.length) {
  for (const p of problems) console.log(`  fail: ${p}`);
  process.exit(1);
}
console.log("  ok: every rule passes");
