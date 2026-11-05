//! Fleet management: discover local Git repositories, sync AgentOS rule files into them,
//! commit strictly the rule files, and create new AgentOS projects.

use std::collections::HashSet;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use anyhow::{anyhow, bail, Context, Result};
use serde::Serialize;
use serde_json::{json, Value};

use crate::constitution::{self, Kind, REPO_RULE_PATHS};
use crate::procs;
use crate::vault::{DelegateArgs, Vault};

const SKIP_ANYWHERE: [&str; 16] = [
    "node_modules",
    ".venv",
    "venv",
    "env",
    "__pycache__",
    ".cache",
    "target",
    "dist",
    "build",
    ".next",
    ".nuxt",
    "site-packages",
    ".git",
    "vendor",
    "obj",
    "bin",
];
const SKIP_TOP: [&str; 14] = [
    "AppData",
    "OneDrive",
    "Music",
    "Videos",
    "Pictures",
    "Saved Games",
    "Searches",
    "Contacts",
    "Favorites",
    "Links",
    "3D Objects",
    "Obsidian",
    "scoop",
    "go",
];
const MAX_DEPTH: usize = 6;
pub const RULES_COMMIT_MSG: &str = "chore(agentos): sync fleet constitution and MCP rules";

#[derive(Debug, Clone, Serialize)]
pub struct RepoInfo {
    pub name: String,
    pub path: String,
    pub stack: Vec<String>,
    pub branch: String,
    pub dirty_files: usize,
    pub remote: String,
    pub status: String,
    pub last_commit: String,
    pub rules_synced: bool,
}

fn git_cmd(repo: &Path, args: &[&str]) -> Result<std::process::Output> {
    let exe = procs::find("git").ok_or_else(|| anyhow!("git is not installed"))?;
    procs::command(&exe)
        .arg("-C")
        .arg(repo)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .context("running git")
}

pub fn git(repo: &Path, args: &[&str]) -> String {
    git_cmd(repo, args)
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim_end().to_string())
        .unwrap_or_default()
}

fn git_ok(repo: &Path, args: &[&str]) -> Result<String> {
    let o = git_cmd(repo, args)?;
    if !o.status.success() {
        bail!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&o.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&o.stdout).trim_end().to_string())
}

fn git_stdin(repo: &Path, args: &[&str], input: &str) -> Result<String> {
    let exe = procs::find("git").ok_or_else(|| anyhow!("git is not installed"))?;
    let mut child = procs::command(&exe)
        .arg("-C")
        .arg(repo)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    child
        .stdin
        .take()
        .ok_or_else(|| anyhow!("no stdin"))?
        .write_all(input.as_bytes())?;
    let o = child.wait_with_output()?;
    if !o.status.success() {
        bail!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&o.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&o.stdout).trim().to_string())
}

/// Find Git repositories under the given roots.
pub fn discover(roots: &[PathBuf]) -> Vec<PathBuf> {
    let home = crate::settings::home();
    let appdata = home.join("AppData");
    let mut found: Vec<PathBuf> = Vec::new();
    for root in roots {
        let top_filter = *root == home;
        let walker = walkdir::WalkDir::new(root)
            .max_depth(MAX_DEPTH + 1)
            .into_iter()
            .filter_entry(|e| {
                if !e.file_type().is_dir() {
                    return false;
                }
                let name = e.file_name().to_string_lossy();
                if e.depth() == 0 {
                    return true;
                }
                if SKIP_ANYWHERE.contains(&name.as_ref()) {
                    return false;
                }
                !(top_filter
                    && e.depth() == 1
                    && (name.starts_with('.') || SKIP_TOP.contains(&name.as_ref())))
            });
        for e in walker.flatten() {
            if e.path().join(".git").is_dir() && e.path() != home {
                found.push(e.path().to_path_buf());
            }
        }
    }
    let mut seen = HashSet::new();
    let mut out: Vec<PathBuf> = found
        .into_iter()
        .filter_map(|p| fs::canonicalize(&p).ok().map(|c| (p, c)))
        .filter(|(_, c)| !c.starts_with(fs::canonicalize(&appdata).unwrap_or(appdata.clone())))
        .filter(|(_, c)| seen.insert(c.clone()))
        .map(|(p, _)| p)
        .collect();
    out.sort_by_key(|p| p.to_string_lossy().to_lowercase());
    out
}

pub fn detect_stack(repo: &Path) -> Vec<String> {
    let mut stack: Vec<String> = Vec::new();
    let add = |s: &str, st: &mut Vec<String>| {
        if !st.iter().any(|x| x == s) {
            st.push(s.to_string());
        }
    };
    let mut dirs = vec![repo.to_path_buf()];
    if let Ok(rd) = fs::read_dir(repo) {
        for e in rd.flatten() {
            let n = e.file_name().to_string_lossy().to_string();
            if e.path().is_dir() && !n.starts_with('.') && !SKIP_ANYWHERE.contains(&n.as_str()) {
                dirs.push(e.path());
            }
        }
    }
    for d in &dirs {
        if let Ok(text) = fs::read_to_string(d.join("package.json")) {
            add("Node.js", &mut stack);
            let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
            let has = |k: &str| {
                v["dependencies"].get(k).is_some() || v["devDependencies"].get(k).is_some()
            };
            for (k, label) in [
                ("typescript", "TypeScript"),
                ("next", "Next.js"),
                ("astro", "Astro"),
                ("react", "React"),
                ("vue", "Vue"),
                ("svelte", "Svelte"),
                ("vite", "Vite"),
                ("tailwindcss", "Tailwind"),
                ("express", "Express"),
                ("electron", "Electron"),
                ("@tauri-apps/api", "Tauri"),
                ("prisma", "Prisma"),
                ("three", "Three.js"),
            ] {
                if has(k) {
                    add(label, &mut stack);
                }
            }
        }
        if d.join("Cargo.toml").exists() {
            add("Rust", &mut stack);
        }
        if d.join("go.mod").exists() {
            add("Go", &mut stack);
        }
        let py: Vec<PathBuf> = ["pyproject.toml", "requirements.txt", "setup.py", "Pipfile"]
            .iter()
            .map(|f| d.join(f))
            .collect();
        if py.iter().any(|p| p.exists()) || d.join("manage.py").exists() {
            add("Python", &mut stack);
            let blob: String = py
                .iter()
                .filter_map(|p| fs::read_to_string(p).ok())
                .collect::<String>()
                .to_lowercase();
            for (k, label) in [
                ("fastapi", "FastAPI"),
                ("django", "Django"),
                ("flask", "Flask"),
            ] {
                if blob.contains(k) || (k == "django" && d.join("manage.py").exists()) {
                    add(label, &mut stack);
                }
            }
        }
        if d.join("Dockerfile").exists() || d.join("docker-compose.yml").exists() {
            add("Docker", &mut stack);
        }
        if d.join("pubspec.yaml").exists() {
            add("Flutter", &mut stack);
        }
    }
    if stack.is_empty() {
        let has_ext = |ext: &str, exclude: &[&str]| {
            fs::read_dir(repo)
                .map(|rd| {
                    rd.flatten().any(|e| {
                        let n = e.file_name().to_string_lossy().to_string();
                        n.ends_with(ext) && !exclude.contains(&n.as_str())
                    })
                })
                .unwrap_or(false)
        };
        if has_ext(".html", &[]) {
            stack.push("Static HTML/CSS/JS".into());
        } else if has_ext(".md", &["CLAUDE.md", "AGENTS.md", "GEMINI.md"]) {
            stack.push("Markdown/docs".into());
        } else {
            stack.push("Unknown".into());
        }
    }
    stack
}

/// True when every managed rule file in the repo matches the current constitution.
pub fn rules_synced(repo: &Path, raw: &str) -> bool {
    REPO_RULE_PATHS.iter().all(|rel| {
        let Ok(text) = fs::read_to_string(repo.join(rel)) else {
            return false;
        };
        let text = text.replace("\r\n", "\n");
        match *rel {
            "CLAUDE.md" => text.contains(&constitution::managed_block(raw, Kind::Claude)),
            "AGENTS.md" => text.contains(&constitution::managed_block(raw, Kind::Agents)),
            "GEMINI.md" => text.contains(&constitution::managed_block(raw, Kind::Gemini)),
            _ => text == constitution::rule_file(raw),
        }
    })
}

fn is_agentos_path(p: &str) -> bool {
    let p = p.trim_matches('"');
    [
        "CLAUDE.md",
        "AGENTS.md",
        "GEMINI.md",
        ".gitignore",
        ".cursorrules",
    ]
    .contains(&p)
        || p.starts_with(".qoder/")
        || p.starts_with(".agents/")
}

pub fn inspect(repo: &Path, raw: &str) -> RepoInfo {
    // Ignore AgentOS rule-sync commits so status reflects real project activity.
    let last = git(
        repo,
        &[
            "log",
            "-1",
            "--format=%cI",
            "--invert-grep",
            "--fixed-strings",
            "--grep=chore(agentos)",
        ],
    );
    let last = if last.is_empty() {
        git(repo, &["log", "-1", "--format=%cI"])
    } else {
        last
    };
    let porcelain = git(repo, &["status", "--porcelain"]);
    let dirty = porcelain
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter(|l| !is_agentos_path(l.split_whitespace().last().unwrap_or_default()))
        .count();
    let status = match chrono::DateTime::parse_from_rfc3339(&last) {
        Err(_) => "no commits".to_string(),
        Ok(t) => {
            let age = (chrono::Utc::now() - t.with_timezone(&chrono::Utc)).num_days();
            if age <= 30 {
                "active"
            } else if age <= 120 {
                "recent"
            } else {
                "dormant"
            }
            .to_string()
        }
    };
    let branch = git(repo, &["rev-parse", "--abbrev-ref", "HEAD"]);
    RepoInfo {
        name: repo
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default(),
        path: repo.display().to_string(),
        stack: detect_stack(repo),
        branch: if branch.is_empty() {
            "(unborn)".into()
        } else {
            branch
        },
        dirty_files: dirty,
        remote: git(repo, &["remote", "get-url", "origin"]),
        status,
        last_commit: last,
        rules_synced: rules_synced(repo, raw),
    }
}

/// Scan roots and inspect every repo in parallel.
pub fn scan(roots: &[PathBuf], raw: &str) -> Vec<RepoInfo> {
    let repos = discover(roots);
    let mut out: Vec<RepoInfo> = std::thread::scope(|s| {
        let handles: Vec<_> = repos
            .chunks(4)
            .map(|chunk| s.spawn(move || chunk.iter().map(|r| inspect(r, raw)).collect::<Vec<_>>()))
            .collect();
        handles
            .into_iter()
            .flat_map(|h| h.join().unwrap_or_default())
            .collect()
    });
    out.sort_by_key(|r| r.name.to_lowercase());
    out
}

#[derive(Debug, Clone, Serialize)]
pub struct SyncResult {
    pub path: String,
    pub name: String,
    pub actions: Vec<(String, String)>,
    pub commit: String,
    pub skipped: Vec<String>,
    pub error: Option<String>,
}

/// Inject the managed rule files into one repo (working tree only).
pub fn inject(repo: &Path, raw: &str) -> Result<Vec<(String, String)>> {
    let mut actions = Vec::new();
    for kind in Kind::REPO {
        let a = constitution::upsert_block(
            &repo.join(kind.file_name()),
            &constitution::managed_block(raw, kind),
        )?;
        actions.push((kind.file_name().to_string(), a.to_string()));
    }
    let rf = constitution::rule_file(raw);
    for rel in [".qoder/rules/agentos.md", ".agents/rules/agentos.md"] {
        actions.push((
            rel.to_string(),
            constitution::write_owned(&repo.join(rel), &rf)?.to_string(),
        ));
    }
    actions.push((
        ".gitignore".into(),
        constitution::ensure_gitignore(repo)?.to_string(),
    ));
    Ok(actions)
}

/// Stage and commit strictly the AgentOS rule files, never the user's other changes.
pub fn commit_rules_only(repo: &Path, raw: &str, message: &str) -> Result<(String, Vec<String>)> {
    let mut skipped = Vec::new();
    let mut staged_paths: Vec<String> = Vec::new();
    let pre_staged: Vec<String> = git(repo, &["diff", "--cached", "--name-only"])
        .lines()
        .map(str::to_string)
        .collect();
    let candidates: Vec<&str> = REPO_RULE_PATHS
        .iter()
        .copied()
        .chain([".gitignore"])
        .collect();
    let mut blobs: Vec<(String, String)> = Vec::new();
    for rel in candidates {
        let path = repo.join(rel);
        if !path.exists() {
            continue;
        }
        if pre_staged.iter().any(|p| p == rel) {
            skipped.push(format!("{rel} (you already staged it)"));
            continue;
        }
        let wt = fs::read_to_string(&path)?.replace("\r\n", "\n");
        let head_raw = git_cmd(repo, &["show", &format!("HEAD:{rel}")])
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).to_string());
        let head_crlf = head_raw
            .as_deref()
            .map(|h| h.contains("\r\n"))
            .unwrap_or(false);
        let head = head_raw.map(|h| h.replace("\r\n", "\n"));
        let desired = constitution::desired_repo_file(raw, rel, head.as_deref());
        if head.is_none() {
            let outside = if rel == ".gitignore" {
                wt.lines()
                    .filter(|l| {
                        !constitution::GI_LINES.contains(&l.trim())
                            && !l.starts_with("# >>> AgentOS")
                            && !l.starts_with("# <<< AgentOS")
                    })
                    .collect::<String>()
            } else {
                constitution::block_re().replace_all(&wt, "").to_string()
            };
            let owned = rel.ends_with("rules/agentos.md");
            if !owned && !outside.trim().is_empty() && wt.trim() != desired.trim() {
                skipped.push(format!(
                    "{rel} (untracked, contains your own uncommitted content)"
                ));
                continue;
            }
        }
        if head.as_deref().map(str::trim) == Some(desired.trim()) {
            continue; // already committed
        }
        let content = if wt.trim() == desired.trim() {
            wt
        } else {
            desired
        };
        // Keep the committed line-ending style so the commit never rewrites whole files.
        let content = if head_crlf {
            content.replace('\n', "\r\n")
        } else {
            content
        };
        blobs.push((rel.to_string(), content));
    }
    if blobs.is_empty() {
        return Ok(("nothing to commit".into(), skipped));
    }
    // Build the commit from HEAD plus our blobs in a private index so user staging is untouched.
    let tmp = tempfile_dir()?;
    let index = tmp.join("index");
    let exe = procs::find("git").ok_or_else(|| anyhow!("git is not installed"))?;
    let run_idx = |args: &[&str]| -> Result<String> {
        let o = procs::command(&exe)
            .arg("-C")
            .arg(repo)
            .args(args)
            .env("GIT_INDEX_FILE", &index)
            .stdin(Stdio::null())
            .output()?;
        if !o.status.success() {
            bail!(
                "git {}: {}",
                args.join(" "),
                String::from_utf8_lossy(&o.stderr).trim()
            );
        }
        Ok(String::from_utf8_lossy(&o.stdout).trim().to_string())
    };
    let has_head = git_cmd(repo, &["rev-parse", "--verify", "HEAD"])
        .map(|o| o.status.success())
        .unwrap_or(false);
    if has_head {
        run_idx(&["read-tree", "HEAD"])?;
    }
    for (rel, content) in &blobs {
        let sha = git_stdin(repo, &["hash-object", "-w", "--stdin"], content)?;
        run_idx(&[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("100644,{sha},{rel}"),
        ])?;
        staged_paths.push(rel.clone());
    }
    let tree = run_idx(&["write-tree"])?;
    let commit = if has_head {
        git_ok(repo, &["commit-tree", &tree, "-p", "HEAD", "-m", message])?
    } else {
        git_ok(repo, &["commit-tree", &tree, "-m", message])?
    };
    let branch_ref = git(repo, &["symbolic-ref", "-q", "HEAD"]);
    let target = if branch_ref.is_empty() {
        "HEAD".to_string()
    } else {
        branch_ref
    };
    git_ok(repo, &["update-ref", &target, &commit])?;
    // Refresh the real index entries for committed paths so they do not show as staged.
    for rel in &staged_paths {
        let _ = git_cmd(repo, &["reset", "-q", "--", rel]);
    }
    let _ = fs::remove_dir_all(&tmp);
    Ok((
        format!(
            "committed {} ({} files)",
            &commit[..8.min(commit.len())],
            staged_paths.len()
        ),
        skipped,
    ))
}

fn tempfile_dir() -> Result<PathBuf> {
    let d = std::env::temp_dir().join(format!("agentos-index-{}", uuid::Uuid::new_v4().simple()));
    fs::create_dir_all(&d)?;
    Ok(d)
}

pub fn sync_fleet_rules(repo_paths: &[String], auto_commit: bool, raw: &str) -> Vec<SyncResult> {
    repo_paths
        .iter()
        .map(|p| {
            let repo = PathBuf::from(p);
            let name = repo
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            let mut res = SyncResult {
                path: p.clone(),
                name,
                actions: vec![],
                commit: "not requested".into(),
                skipped: vec![],
                error: None,
            };
            if !repo.join(".git").exists() {
                res.error = Some("not a git repository".into());
                return res;
            }
            match inject(&repo, raw) {
                Ok(a) => res.actions = a,
                Err(e) => {
                    res.error = Some(format!("{e:#}"));
                    return res;
                }
            }
            if auto_commit {
                match commit_rules_only(&repo, raw, RULES_COMMIT_MSG) {
                    Ok((c, s)) => {
                        res.commit = c;
                        res.skipped = s;
                    }
                    Err(e) => res.error = Some(format!("{e:#}")),
                }
            }
            res
        })
        .collect()
}

/// Write or refresh `01_Projects/<name>.md`, preserving a trailing `## Notes` section.
pub fn write_project_note(vault: &Vault, info: &RepoInfo) -> Result<PathBuf> {
    let safe: String = info
        .name
        .chars()
        .map(|c| {
            if "\\/:*?\"<>|#^[]".contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect();
    let path = vault.projects().join(format!("{safe}.md"));
    let keep = fs::read_to_string(&path)
        .ok()
        .and_then(|old| {
            old.find("\n## Notes")
                .map(|i| old[i + 1..].trim().to_string())
        })
        .unwrap_or_else(|| {
            "## Notes\n\n_Human and agent notes. This section is preserved on re-index._".into()
        });
    let q = |s: &str| serde_json::to_string(s).unwrap_or_default();
    let stack_yaml = format!(
        "[{}]",
        info.stack
            .iter()
            .map(|s| q(s))
            .collect::<Vec<_>>()
            .join(", ")
    );
    let body = format!(
        "---\nproject: {}\npath: {}\nstack: {stack_yaml}\nstatus: {}\nbranch: {}\nremote: {}\nlast_commit: {}\nagentos: v2\nindexed: {}\ntags: [project, repo]\n---\n\n# {}\n\n> [!info] Local repository\n> `{}`\n\n| Field | Value |\n|---|---|\n| Stack | {} |\n| Status | {} (uncommitted files at index time, excluding AgentOS rule files: {}) |\n| Branch | `{}` |\n| Remote | {} |\n| Last commit | {} |\n| Agent rules | CLAUDE.md, AGENTS.md, GEMINI.md, .qoder/rules/agentos.md, .agents/rules/agentos.md (AgentOS v2) |\n\nRules: [[SYSTEM_CONTEXT]] - Decisions: [[DECISION_LOG]]\n\n{keep}\n",
        q(&info.name), q(&info.path), info.status, q(&info.branch), q(if info.remote.is_empty() { "(none)" } else { &info.remote }),
        q(if info.last_commit.is_empty() { "(none)" } else { &info.last_commit }), crate::vault::now(),
        info.name, info.path, info.stack.join(", "), info.status, info.dirty_files, info.branch,
        if info.remote.is_empty() { "(none)" } else { &info.remote }, if info.last_commit.is_empty() { "(none)" } else { &info.last_commit },
    );
    fs::create_dir_all(vault.projects())?;
    fs::write(&path, constitution::undash(&body))?;
    Ok(path)
}

fn write_if_missing(path: &Path, content: &str) -> Result<()> {
    if !path.exists() {
        if let Some(p) = path.parent() {
            fs::create_dir_all(p)?;
        }
        fs::write(path, content)?;
    }
    Ok(())
}

/// Lay down a minimal, working starter for the chosen stack.
fn apply_template(dir: &Path, name: &str, template: &str) -> Result<String> {
    let pkg_name: String = name
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let pkg_name = pkg_name.trim_matches('-').to_string();
    match template {
        "rust" => {
            if let Some(cargo) = procs::find("cargo") {
                let o = procs::command(&cargo)
                    .args([
                        "init",
                        "--vcs",
                        "none",
                        "--name",
                        &pkg_name.replace('-', "_"),
                    ])
                    .current_dir(dir)
                    .output()?;
                if !o.status.success() {
                    bail!("cargo init failed: {}", String::from_utf8_lossy(&o.stderr));
                }
                return Ok("cargo init".into());
            }
            write_if_missing(&dir.join("Cargo.toml"), &format!("[package]\nname = \"{}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\n", pkg_name.replace('-', "_")))?;
            write_if_missing(
                &dir.join("src/main.rs"),
                "fn main() {\n    println!(\"Hello from AgentOS\");\n}\n",
            )?;
            write_if_missing(&dir.join(".gitignore"), "/target\n")?;
            Ok("Rust starter files".into())
        }
        "typescript" => {
            write_if_missing(&dir.join("package.json"), &format!(
                "{{\n  \"name\": \"{pkg_name}\",\n  \"version\": \"0.1.0\",\n  \"type\": \"module\",\n  \"scripts\": {{\n    \"build\": \"tsc\",\n    \"start\": \"node dist/index.js\"\n  }},\n  \"devDependencies\": {{\n    \"typescript\": \"^5.6.0\"\n  }}\n}}\n"))?;
            write_if_missing(&dir.join("tsconfig.json"), "{\n  \"compilerOptions\": {\n    \"target\": \"ES2022\",\n    \"module\": \"NodeNext\",\n    \"moduleResolution\": \"NodeNext\",\n    \"strict\": true,\n    \"outDir\": \"dist\",\n    \"rootDir\": \"src\"\n  },\n  \"include\": [\"src\"]\n}\n")?;
            write_if_missing(&dir.join("src/index.ts"), "export function main(): void {\n  console.log(\"Hello from AgentOS\");\n}\n\nmain();\n")?;
            write_if_missing(&dir.join(".gitignore"), "node_modules/\ndist/\n")?;
            Ok("TypeScript starter files".into())
        }
        "python" => {
            if let Some(uv) = procs::find("uv") {
                let o = procs::command(&uv)
                    .args([
                        "init",
                        "--no-workspace",
                        "--vcs",
                        "none",
                        "--name",
                        &pkg_name,
                    ])
                    .current_dir(dir)
                    .output()?;
                if o.status.success() {
                    return Ok("uv init".into());
                }
            }
            write_if_missing(&dir.join("pyproject.toml"), &format!(
                "[project]\nname = \"{pkg_name}\"\nversion = \"0.1.0\"\nrequires-python = \">=3.12\"\ndependencies = []\n\n[tool.ruff]\nline-length = 100\n"))?;
            write_if_missing(&dir.join("main.py"), "def main() -> None:\n    print(\"Hello from AgentOS\")\n\n\nif __name__ == \"__main__\":\n    main()\n")?;
            write_if_missing(&dir.join(".gitignore"), ".venv/\n__pycache__/\n")?;
            Ok("Python starter files".into())
        }
        "empty" | "" => Ok("no template".into()),
        other => bail!("Unknown stack template '{other}'. Use empty, rust, typescript or python."),
    }
}

/// Create a new AgentOS project end to end.
pub fn create_project(
    vault: &Vault,
    name: &str,
    parent_dir: &str,
    template: &str,
    create_github: bool,
    private: bool,
    raw: &str,
) -> Result<Value> {
    let clean = name.trim();
    if clean.is_empty() || clean.contains(['/', '\\', ':', '*', '?', '"', '<', '>', '|']) {
        bail!("Project name must be a plain folder name");
    }
    let dir = PathBuf::from(parent_dir).join(clean);
    if dir.join(".git").exists() {
        bail!("{} is already a git repository", dir.display());
    }
    fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let mut steps = vec![format!("created {}", dir.display())];
    git_ok(&dir, &["init", "-b", "main"])?;
    steps.push("git init -b main".into());
    steps.push(apply_template(&dir, clean, template)?);
    write_if_missing(&dir.join("README.md"), &format!(
        "# {clean}\n\nBootstrapped with AgentOS Studio. Agent rules live in `CLAUDE.md`, `AGENTS.md` and `GEMINI.md`; shared memory and the task bus are served by the `agentos-hub` MCP server.\n"))?;
    inject(&dir, raw)?;
    steps.push("injected AgentOS rule files".into());
    git_ok(&dir, &["add", "-A"])?;
    let commit = git_cmd(
        &dir,
        &[
            "commit",
            "-m",
            "chore(repo): bootstrap project with AgentOS rules",
        ],
    )?;
    if !commit.status.success() {
        bail!(
            "initial commit failed: {}",
            String::from_utf8_lossy(&commit.stderr).trim()
        );
    }
    steps.push("initial commit".into());
    let mut github_url = Value::Null;
    if create_github {
        let gh = procs::find("gh").ok_or_else(|| anyhow!("GitHub CLI (gh) is not installed"))?;
        let vis = if private { "--private" } else { "--public" };
        let o = procs::command(&gh)
            .args([
                "repo", "create", clean, vis, "--source", ".", "--remote", "origin", "--push",
            ])
            .current_dir(&dir)
            .stdin(Stdio::null())
            .output()?;
        if !o.status.success() {
            bail!(
                "gh repo create failed: {}",
                String::from_utf8_lossy(&o.stderr).trim()
            );
        }
        let url = git(&dir, &["remote", "get-url", "origin"]);
        steps.push(format!("GitHub repository {url}"));
        github_url = json!(url);
    }
    let info = inspect(&dir, raw);
    let note = write_project_note(vault, &info)?;
    steps.push(format!("vault note {}", note.display()));
    let task = vault.delegate_task(DelegateArgs {
        title: format!("Sync new project {clean} to Notion"),
        assigned_to: "hermes".into(),
        creator: "agentos-studio".into(),
        project: clean.into(),
        context_files: vec![note.display().to_string(), "03_Memory/SYSTEM_CONTEXT.md".into()],
        instructions: format!(
            "A new project was created at {} (stack: {}). Claim this task, then append a row for '{clean}' (path, stack, status, remote) to the 'Repositories' section of the Notion page 'AgentOS Command Center'. Then complete_task.",
            dir.display(), info.stack.join(", ")),
        priority: "medium".into(),
    })?;
    steps.push(format!(
        "queued Notion sync as {}",
        task["id"].as_str().unwrap_or_default()
    ));
    Ok(
        json!({"path": dir.display().to_string(), "repo": info, "github": github_url, "note": note.display().to_string(), "notion_task": task, "steps": steps}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const RAW: &str = "## AgentOS Constitution v2\n\n### Law 1 - Full autonomy\n- one\n";

    fn repo() -> (tempfile::TempDir, PathBuf) {
        let d = tempfile::tempdir().unwrap();
        let r = d.path().join("demo");
        fs::create_dir_all(&r).unwrap();
        git_ok(&r, &["init", "-b", "main"]).unwrap();
        git_ok(&r, &["config", "user.email", "t@example.com"]).unwrap();
        git_ok(&r, &["config", "user.name", "Test"]).unwrap();
        git_ok(&r, &["config", "commit.gpgsign", "false"]).unwrap();
        fs::write(r.join("CLAUDE.md"), "# Demo\n\nUser rules.\n").unwrap();
        fs::write(r.join("app.txt"), "v1\n").unwrap();
        git_ok(&r, &["add", "-A"]).unwrap();
        git_ok(&r, &["commit", "-m", "init"]).unwrap();
        (d, r)
    }

    #[test]
    fn inject_then_commit_only_rule_files() {
        let (_d, r) = repo();
        fs::write(r.join("app.txt"), "v2 user work\n").unwrap();
        fs::write(r.join("notes.txt"), "untracked user file\n").unwrap();
        fs::write(r.join("staged.txt"), "user staged\n").unwrap();
        git_ok(&r, &["add", "staged.txt"]).unwrap();
        inject(&r, RAW).unwrap();
        assert!(rules_synced(&r, RAW));
        let (c, _) = commit_rules_only(&r, RAW, RULES_COMMIT_MSG).unwrap();
        assert!(c.starts_with("committed"), "{c}");
        let files = git(&r, &["show", "--name-only", "--format=", "HEAD"]);
        let mut names: Vec<&str> = files.lines().collect();
        names.sort();
        assert_eq!(
            names,
            [
                ".agents/rules/agentos.md",
                ".gitignore",
                ".qoder/rules/agentos.md",
                "AGENTS.md",
                "CLAUDE.md",
                "GEMINI.md"
            ]
        );
        let status = git(&r, &["status", "--porcelain"]);
        assert!(status.contains(" M app.txt"), "{status}");
        assert!(status.contains("?? notes.txt"));
        assert!(status.contains("A  staged.txt"));
        assert!(!status.contains("CLAUDE.md"));
        let (again, _) = commit_rules_only(&r, RAW, RULES_COMMIT_MSG).unwrap();
        assert_eq!(again, "nothing to commit");
    }

    #[test]
    fn stack_detection() {
        let (_d, r) = repo();
        fs::write(r.join("Cargo.toml"), "[package]\nname=\"x\"\n").unwrap();
        fs::write(
            r.join("package.json"),
            r#"{"dependencies":{"react":"19"},"devDependencies":{"typescript":"5"}}"#,
        )
        .unwrap();
        let s = detect_stack(&r);
        for want in ["Node.js", "TypeScript", "React", "Rust"] {
            assert!(s.contains(&want.to_string()), "{s:?}");
        }
    }

    #[test]
    fn create_project_end_to_end() {
        let d = tempfile::tempdir().unwrap();
        let v = Vault::new(d.path().join("vault"));
        v.scaffold().unwrap();
        let parent = d.path().join("code");
        fs::create_dir_all(&parent).unwrap();
        std::env::set_var("GIT_AUTHOR_NAME", "Test");
        std::env::set_var("GIT_AUTHOR_EMAIL", "t@example.com");
        std::env::set_var("GIT_COMMITTER_NAME", "Test");
        std::env::set_var("GIT_COMMITTER_EMAIL", "t@example.com");
        let out = create_project(
            &v,
            "demo-app",
            parent.to_str().unwrap(),
            "typescript",
            false,
            true,
            RAW,
        )
        .unwrap();
        let dir = PathBuf::from(out["path"].as_str().unwrap());
        assert!(dir.join("src/index.ts").exists());
        assert!(rules_synced(&dir, RAW));
        assert_eq!(git(&dir, &["status", "--porcelain"]), "");
        assert!(v.projects().join("demo-app.md").exists());
        assert_eq!(v.board().pending.len(), 1);
    }
}
