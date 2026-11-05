// Law 2 gate: fail if any tracked text file contains an em-dash (U+2014) or en-dash (U+2013).
import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";

const files = execFileSync("git", ["ls-files", "-co", "--exclude-standard"], { encoding: "utf8" })
  .split("\n")
  .filter((f) => f && !/\.(png|ico|icns|jpg|jpeg|gif|webp|woff2?)$/i.test(f) && !f.startsWith("node_modules/"));
const bad = [];
for (const f of files) {
  let text;
  try {
    text = readFileSync(f, "utf8");
  } catch {
    continue;
  }
  text.split("\n").forEach((line, i) => {
    if (/[\u2013\u2014]/.test(line)) bad.push(`${f}:${i + 1}: ${line.trim().slice(0, 120)}`);
  });
}
if (bad.length) {
  console.error(`Found ${bad.length} line(s) with em-dashes or en-dashes:\n` + bad.join("\n"));
  process.exit(1);
}
console.log(`check-dashes: ${files.length} files clean`);
