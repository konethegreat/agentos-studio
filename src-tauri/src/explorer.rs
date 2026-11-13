//! Explorer: a read-mostly window into a repository, so the owner can see what the agents see
//! without leaving Studio. The file tree, a code viewer, search, and the working changes with
//! their diffs, for the repository itself or any of its worktrees (one per mission attempt).
//!
//! Not an editor: there is no language server, no debugger and no tabs of unsaved buffers. Heavy
//! editing goes to VS Code with one click (`open_in_editor`).
//!
//! Safety: every call names a root and a path inside it. The root must be a Git checkout under
//! one of Studio's scan roots, and the path must stay inside the root after it is resolved, so a
//! `..` or a link out of the repository is refused.

use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;

use anyhow::{anyhow, bail, Context, Result};
use serde::Serialize;

use crate::procs;

/// Files listed at most, so a monorepo cannot flood the window.
pub const MAX_FILES: usize = 20_000;
/// Bytes of one file shown at most.
pub const MAX_VIEW_BYTES: u64 = 512 * 1024;
/// Search hits returned at most.
pub const MAX_HITS: usize = 500;
/// Characters of one diff returned at most.
pub const MAX_DIFF_CHARS: usize = 400_000;

/// Folders never listed outside Git (inside Git, `.gitignore` decides).
const SKIP: [&str; 10] = [
    ".git",
    "node_modules",
    "target",
    "dist",
    "build",
    ".next",
    ".venv",
    "__pycache__",
    ".worktrees",
    ".cache",
];

#[derive(Debug, Serialize, Clone, PartialEq)]
pub struct FileList {
    /// Paths relative to the root, with forward slashes, sorted.
    pub files: Vec<String>,
    pub truncated: bool,
    /// Listed by Git (so `.gitignore` applies) rather than by walking the folder.
    pub git: bool,
    pub branch: String,
}

#[derive(Debug, Serialize, Clone, PartialEq)]
pub struct FileView {
    pub path: String,
    pub size: u64,
    pub binary: bool,
    pub truncated: bool,
    pub text: String,
}

#[derive(Debug, Serialize, Clone, PartialEq)]
pub struct Change {
    pub path: String,
    /// The two-letter porcelain code, for example ` M`, `A `, `??`, `R `.
    pub code: String,
    /// Plain words for the code: modified, added, deleted, renamed, untracked, conflict.
    pub kind: String,
    /// The old path of a rename.
    pub from: Option<String>,
}

#[derive(Debug, Serialize, Clone, PartialEq)]
pub struct Hit {
    pub path: String,
    pub line: u32,
    pub text: String,
}

#[derive(Debug, Serialize, Clone, PartialEq)]
pub struct Checkout {
    pub path: String,
    pub branch: String,
    /// The main checkout rather than a worktree.
    pub main: bool,
}

fn git_out(root: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let exe = procs::find("git").ok_or_else(|| anyhow!("git is not installed"))?;
    let o = procs::command(&exe)
        .arg("-C")
        .arg(root)
        .args(["-c", "core.quotepath=off"])
        .args(args)
        .stdin(Stdio::null())
        .output()
        .context("running git")?;
    // `git grep` exits 1 when nothing matched: that is an empty answer, not a failure.
    if !o.status.success() && !(args.first() == Some(&"grep") && o.status.code() == Some(1)) {
        bail!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&o.stderr).trim()
        );
    }
    Ok(o.stdout)
}

fn git_text(root: &Path, args: &[&str]) -> Result<String> {
    Ok(String::from_utf8_lossy(&git_out(root, args)?).into_owned())
}

fn slashes(p: &str) -> String {
    p.replace('\\', "/")
}

/// The root as a canonical folder, refused unless it is a Git checkout (a repository or a
/// worktree) under one of `allowed`.
pub fn check_root(root: &str, allowed: &[PathBuf]) -> Result<PathBuf> {
    let p = Path::new(root);
    if !p.is_dir() {
        bail!("{root} is not a folder");
    }
    let canon = dunce(p)?;
    if !canon.join(".git").exists() {
        bail!("{root} is not a Git checkout");
    }
    let inside = allowed
        .iter()
        .filter_map(|a| dunce(a).ok())
        .any(|a| canon.starts_with(&a));
    if !inside {
        bail!("{root} is outside the folders Studio scans");
    }
    Ok(canon)
}

/// Canonical path without the `\\?\` prefix Windows adds, so prefixes compare cleanly.
fn dunce(p: &Path) -> Result<PathBuf> {
    let c = std::fs::canonicalize(p).with_context(|| format!("{} not found", p.display()))?;
    let s = c.to_string_lossy();
    Ok(match s.strip_prefix(r"\\?\") {
        Some(rest) if !rest.starts_with("UNC\\") => PathBuf::from(rest),
        _ => c,
    })
}

/// A path inside the root. Refuses absolute paths, `..`, and anything that resolves outside.
pub fn inside(root: &Path, rel: &str) -> Result<PathBuf> {
    let rel_path = Path::new(rel);
    if rel.trim().is_empty() {
        bail!("no path given");
    }
    if rel_path.is_absolute()
        || rel_path
            .components()
            .any(|c| !matches!(c, Component::Normal(_) | Component::CurDir))
    {
        bail!("{rel} is not a path inside the repository");
    }
    let joined = root.join(rel_path);
    // A file that no longer exists (a deletion in the diff) cannot be canonicalised: its parent
    // must be inside instead.
    let probe = if joined.exists() {
        dunce(&joined)?
    } else {
        let parent = joined.parent().unwrap_or(root);
        let mut p = if parent.exists() {
            dunce(parent)?
        } else {
            root.to_path_buf()
        };
        if let Some(name) = joined.file_name() {
            p.push(name);
        }
        p
    };
    if !probe.starts_with(root) {
        bail!("{rel} resolves outside the repository");
    }
    Ok(joined)
}

/// Every file in the checkout: tracked and untracked-but-not-ignored when Git is there.
pub fn files(root: &Path) -> Result<FileList> {
    let branch = git_text(root, &["branch", "--show-current"])
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    match git_out(
        root,
        &[
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "-z",
        ],
    ) {
        Ok(raw) => {
            let mut files: Vec<String> = raw
                .split(|b| *b == 0)
                .filter(|s| !s.is_empty())
                .map(|s| slashes(&String::from_utf8_lossy(s)))
                // A deleted but still tracked file is listed by Git and missing on disk.
                .filter(|f| root.join(f).exists())
                .collect();
            files.sort();
            files.dedup();
            let truncated = files.len() > MAX_FILES;
            files.truncate(MAX_FILES);
            Ok(FileList {
                files,
                truncated,
                git: true,
                branch,
            })
        }
        Err(_) => {
            let (files, truncated) = walk(root);
            Ok(FileList {
                files,
                truncated,
                git: false,
                branch,
            })
        }
    }
}

fn walk(root: &Path) -> (Vec<String>, bool) {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            let p = e.path();
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_dir() {
                if !SKIP.contains(&name.as_str()) {
                    stack.push(p);
                }
            } else if ft.is_file() {
                if let Ok(rel) = p.strip_prefix(root) {
                    out.push(slashes(&rel.to_string_lossy()));
                }
                if out.len() > MAX_FILES {
                    out.sort();
                    out.truncate(MAX_FILES);
                    return (out, true);
                }
            }
        }
    }
    out.sort();
    (out, false)
}

/// A file's text for the viewer: binary files and anything past `MAX_VIEW_BYTES` are flagged.
pub fn read(root: &Path, rel: &str) -> Result<FileView> {
    let p = inside(root, rel)?;
    let meta = std::fs::metadata(&p).with_context(|| format!("{rel} not found"))?;
    if !meta.is_file() {
        bail!("{rel} is not a file");
    }
    let size = meta.len();
    let mut buf = Vec::new();
    std::fs::File::open(&p)?
        .take(MAX_VIEW_BYTES)
        .read_to_end(&mut buf)?;
    let binary = looks_binary(&buf);
    Ok(FileView {
        path: slashes(rel),
        size,
        binary,
        truncated: size > MAX_VIEW_BYTES,
        text: if binary {
            String::new()
        } else {
            String::from_utf8_lossy(&buf).into_owned()
        },
    })
}

/// A NUL byte in the first 8 KB means binary, as Git decides it. Pure.
pub fn looks_binary(bytes: &[u8]) -> bool {
    bytes.iter().take(8192).any(|b| *b == 0)
}

/// Plain words for a porcelain status code. Pure.
pub fn kind_of(code: &str) -> &'static str {
    let mut c = code.chars();
    let (x, y) = (c.next().unwrap_or(' '), c.next().unwrap_or(' '));
    match (x, y) {
        ('?', '?') => "untracked",
        ('U', _) | (_, 'U') | ('A', 'A') | ('D', 'D') => "conflict",
        ('R', _) | (_, 'R') => "renamed",
        ('C', _) | (_, 'C') => "copied",
        ('A', _) | (_, 'A') => "added",
        ('D', _) | (_, 'D') => "deleted",
        _ => "modified",
    }
}

/// Parse `git status --porcelain=v1 -z`. Pure.
pub fn parse_status(raw: &[u8]) -> Vec<Change> {
    let parts: Vec<String> = raw
        .split(|b| *b == 0)
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < parts.len() {
        let entry = &parts[i];
        i += 1;
        if entry.len() < 4 {
            continue;
        }
        let code = entry[..2].to_string();
        let path = slashes(&entry[3..]);
        // A rename or copy is followed by its source path.
        let from = if code.contains('R') || code.contains('C') {
            let f = parts.get(i).map(|s| slashes(s));
            i += 1;
            f
        } else {
            None
        };
        out.push(Change {
            kind: kind_of(&code).to_string(),
            code,
            path,
            from,
        });
    }
    out
}

/// The working changes against the last commit, untracked files included one by one.
pub fn status(root: &Path) -> Result<Vec<Change>> {
    Ok(parse_status(&git_out(
        root,
        &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
    )?))
}

/// The unified diff of one changed file. An untracked file is shown as wholly added.
pub fn diff(root: &Path, rel: &str) -> Result<String> {
    let p = inside(root, rel)?;
    let tracked = git_out(root, &["ls-files", "--error-unmatch", "--", rel]).is_ok();
    let mut text = if tracked {
        let has_head = git_out(root, &["rev-parse", "--verify", "-q", "HEAD"]).is_ok();
        if has_head {
            git_text(root, &["diff", "--no-color", "HEAD", "--", rel])?
        } else {
            git_text(root, &["diff", "--no-color", "--cached", "--", rel])?
        }
    } else {
        let view = read(root, rel)?;
        if view.binary {
            format!("Binary file {rel} added\n")
        } else {
            added_diff(rel, &view.text)
        }
    };
    if text.trim().is_empty() && !p.exists() {
        text = format!("{rel} was deleted\n");
    }
    if text.chars().count() > MAX_DIFF_CHARS {
        text = text.chars().take(MAX_DIFF_CHARS).collect();
        text.push_str("\n... diff cut here; open the file in VS Code for the rest\n");
    }
    Ok(text)
}

/// A new file as a unified diff. Pure.
pub fn added_diff(rel: &str, text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let mut out = format!(
        "new file\n--- /dev/null\n+++ b/{rel}\n@@ -0,0 +1,{} @@\n",
        lines.len()
    );
    for l in lines {
        out.push('+');
        out.push_str(l);
        out.push('\n');
    }
    out
}

/// Parse `git grep -n -z` output (`path\0line\0text`). Pure.
pub fn parse_grep(raw: &[u8], max: usize) -> Vec<Hit> {
    let text = String::from_utf8_lossy(raw);
    let mut out = Vec::new();
    for line in text.lines() {
        let mut parts = line.splitn(3, '\0');
        let (Some(path), Some(n), Some(body)) = (parts.next(), parts.next(), parts.next()) else {
            continue;
        };
        let Ok(n) = n.parse::<u32>() else { continue };
        out.push(Hit {
            path: slashes(path),
            line: n,
            text: body.trim().chars().take(300).collect(),
        });
        if out.len() >= max {
            break;
        }
    }
    out
}

/// Text search across the checkout, ignored files excluded. Literal, case-insensitive unless
/// `case` is set.
pub fn search(root: &Path, query: &str, case: bool) -> Result<Vec<Hit>> {
    let q = query.trim();
    if q.chars().count() < 2 {
        return Ok(Vec::new());
    }
    let mut args = vec![
        "grep",
        "-n",
        "-z",
        "-I",
        "-F",
        "--no-color",
        "--untracked",
        "--exclude-standard",
    ];
    if !case {
        args.push("-i");
    }
    args.extend(["-e", q, "--", "."]);
    Ok(parse_grep(&git_out(root, &args)?, MAX_HITS))
}

/// Parse `git worktree list --porcelain`. Pure.
pub fn parse_worktrees(text: &str) -> Vec<Checkout> {
    let mut out: Vec<Checkout> = Vec::new();
    for block in text.split("\n\n") {
        let mut path = None;
        let mut branch = String::new();
        let mut bare = false;
        for l in block.lines() {
            if let Some(p) = l.strip_prefix("worktree ") {
                path = Some(slashes(p.trim()));
            } else if let Some(b) = l.strip_prefix("branch ") {
                branch = b.trim().trim_start_matches("refs/heads/").to_string();
            } else if l.trim() == "detached" {
                branch = "(detached)".into();
            } else if l.trim() == "bare" {
                bare = true;
            }
        }
        if let (Some(path), false) = (path, bare) {
            let main = out.is_empty();
            out.push(Checkout { path, branch, main });
        }
    }
    out
}

/// The repository and every worktree of it that still exists on disk.
pub fn checkouts(root: &Path) -> Result<Vec<Checkout>> {
    let list = parse_worktrees(&git_text(root, &["worktree", "list", "--porcelain"])?);
    Ok(list
        .into_iter()
        .filter(|c| Path::new(&c.path).is_dir())
        .collect())
}

/// The arguments that open a file at a line, or a folder, in VS Code. Pure.
pub fn editor_args(root: &Path, file: Option<&Path>, line: Option<u32>) -> Vec<String> {
    match file {
        Some(f) => vec![
            root.display().to_string(),
            "-g".into(),
            format!("{}:{}", f.display(), line.unwrap_or(1).max(1)),
        ],
        None => vec![root.display().to_string()],
    }
}

/// Open the root (and a file at a line) in VS Code.
pub fn open_in_editor(root: &Path, rel: Option<&str>, line: Option<u32>) -> Result<()> {
    let file = match rel.filter(|r| !r.trim().is_empty()) {
        Some(r) => Some(inside(root, r)?),
        None => None,
    };
    let exe = procs::find("code")
        .ok_or_else(|| anyhow!("VS Code's `code` command is not on PATH. In VS Code run: Shell Command: Install 'code' command in PATH"))?;
    procs::command(&exe)
        .args(editor_args(root, file.as_deref(), line))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("starting VS Code")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(dir: &Path, args: &[&str]) {
        let o = procs::command(&procs::find("git").expect("git"))
            .arg("-C")
            .arg(dir)
            .args([
                "-c",
                "user.email=test@example.com",
                "-c",
                "user.name=test",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .output()
            .unwrap();
        assert!(
            o.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
    }

    fn repo() -> (tempfile::TempDir, PathBuf) {
        let d = tempfile::tempdir().unwrap();
        let root = dunce(d.path()).unwrap().join("r");
        std::fs::create_dir_all(root.join("src")).unwrap();
        git(&root, &["init", "-q", "-b", "main"]);
        std::fs::write(root.join(".gitignore"), "ignored/\n").unwrap();
        std::fs::write(
            root.join("src/main.rs"),
            "fn main() {\n    println!(\"hi\");\n}\n",
        )
        .unwrap();
        std::fs::write(root.join("README.md"), "# Title\n").unwrap();
        git(&root, &["add", "."]);
        git(&root, &["commit", "-q", "-m", "first"]);
        (d, root)
    }

    #[test]
    fn the_tree_lists_tracked_and_new_files_but_not_ignored_ones() {
        let (_d, root) = repo();
        std::fs::create_dir_all(root.join("ignored")).unwrap();
        std::fs::write(root.join("ignored/secret.txt"), "x").unwrap();
        std::fs::write(root.join("new.ts"), "export {}\n").unwrap();
        let list = files(&root).unwrap();
        assert!(list.git);
        assert_eq!(list.branch, "main");
        assert_eq!(
            list.files,
            vec![".gitignore", "README.md", "new.ts", "src/main.rs"]
        );
    }

    #[test]
    fn paths_outside_the_repository_are_refused() {
        let (_d, root) = repo();
        assert!(inside(&root, "src/main.rs").is_ok());
        assert!(inside(&root, "../outside.txt").is_err());
        assert!(inside(&root, "src/../../outside.txt").is_err());
        assert!(inside(&root, "").is_err());
        let abs = root.join("README.md").display().to_string();
        assert!(inside(&root, &abs).is_err(), "absolute paths are refused");
        assert!(read(&root, "../r/README.md").is_err());
    }

    #[test]
    fn a_root_must_be_a_checkout_under_the_scan_roots() {
        let (d, root) = repo();
        let allowed = vec![d.path().to_path_buf()];
        assert_eq!(
            check_root(&root.display().to_string(), &allowed).unwrap(),
            root
        );
        assert!(check_root(&root.join("src").display().to_string(), &allowed).is_err());
        let elsewhere = tempfile::tempdir().unwrap();
        assert!(check_root(
            &root.display().to_string(),
            &[elsewhere.path().to_path_buf()]
        )
        .is_err());
    }

    #[test]
    fn the_viewer_reads_text_and_flags_binary_files() {
        let (_d, root) = repo();
        let v = read(&root, "src/main.rs").unwrap();
        assert!(!v.binary && !v.truncated);
        assert!(v.text.contains("println!"));
        std::fs::write(root.join("logo.png"), [137u8, 80, 78, 71, 0, 0, 1]).unwrap();
        let b = read(&root, "logo.png").unwrap();
        assert!(b.binary);
        assert!(b.text.is_empty());
    }

    #[test]
    fn changes_and_their_diffs_are_shown() {
        let (_d, root) = repo();
        std::fs::write(
            root.join("src/main.rs"),
            "fn main() {\n    println!(\"hello\");\n}\n",
        )
        .unwrap();
        std::fs::write(root.join("notes.txt"), "one\ntwo\n").unwrap();
        std::fs::remove_file(root.join("README.md")).unwrap();
        let mut changes = status(&root).unwrap();
        changes.sort_by(|a, b| a.path.cmp(&b.path));
        let kinds: Vec<(&str, &str)> = changes
            .iter()
            .map(|c| (c.path.as_str(), c.kind.as_str()))
            .collect();
        assert_eq!(
            kinds,
            vec![
                ("README.md", "deleted"),
                ("notes.txt", "untracked"),
                ("src/main.rs", "modified")
            ]
        );
        let d = diff(&root, "src/main.rs").unwrap();
        assert!(d.contains("-    println!(\"hi\");"), "{d}");
        assert!(d.contains("+    println!(\"hello\");"), "{d}");
        let new = diff(&root, "notes.txt").unwrap();
        assert!(new.contains("+one") && new.contains("+two"), "{new}");
        let gone = diff(&root, "README.md").unwrap();
        assert!(gone.contains("-# Title"), "{gone}");
    }

    #[test]
    fn search_finds_lines_case_insensitively_and_skips_ignored_files() {
        let (_d, root) = repo();
        std::fs::create_dir_all(root.join("ignored")).unwrap();
        std::fs::write(root.join("ignored/x.txt"), "PRINTLN here\n").unwrap();
        let hits = search(&root, "PRINTLN", false).unwrap();
        assert_eq!(hits.len(), 1, "{hits:?}");
        assert_eq!(hits[0].path, "src/main.rs");
        assert_eq!(hits[0].line, 2);
        assert!(search(&root, "PRINTLN", true).unwrap().is_empty());
        assert!(search(&root, "nothing like this", false)
            .unwrap()
            .is_empty());
        assert!(search(&root, "x", false).unwrap().is_empty(), "too short");
    }

    #[test]
    fn porcelain_status_and_worktrees_parse() {
        let raw = b" M src/a.rs\0R  new.rs\0old.rs\0?? x.txt\0UU c.rs\0";
        let c = parse_status(raw);
        assert_eq!(c.len(), 4);
        assert_eq!(c[0].kind, "modified");
        assert_eq!(c[1].kind, "renamed");
        assert_eq!(c[1].from.as_deref(), Some("old.rs"));
        assert_eq!(c[2].kind, "untracked");
        assert_eq!(c[3].kind, "conflict");
        let wt = parse_worktrees(
            "worktree C:/r\nHEAD abc\nbranch refs/heads/main\n\nworktree C:/r/.worktrees/m-1-2\nHEAD def\nbranch refs/heads/agentos/m-1-2\n\nworktree C:/r/.worktrees/x\nHEAD 123\ndetached\n",
        );
        assert_eq!(wt.len(), 3);
        assert!(wt[0].main && !wt[1].main);
        assert_eq!(wt[1].branch, "agentos/m-1-2");
        assert_eq!(wt[2].branch, "(detached)");
    }

    #[test]
    fn worktrees_of_a_repository_are_listed() {
        let (_d, root) = repo();
        let wt = root.join(".worktrees").join("m-9-1");
        git(
            &root,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "agentos/m-9-1",
                &wt.display().to_string(),
            ],
        );
        let list = checkouts(&root).unwrap();
        assert_eq!(list.len(), 2, "{list:?}");
        assert!(list[0].main);
        assert_eq!(list[1].branch, "agentos/m-9-1");
    }

    #[test]
    fn vs_code_opens_the_folder_and_the_file_at_its_line() {
        let root = Path::new("C:/r");
        assert_eq!(editor_args(root, None, None), vec!["C:/r".to_string()]);
        let f = root.join("src/a.rs");
        let a = editor_args(root, Some(&f), Some(12));
        assert_eq!(a[1], "-g");
        assert!(a[2].ends_with(":12"), "{a:?}");
        assert!(editor_args(root, Some(&f), None)[2].ends_with(":1"));
        assert_eq!(kind_of("A "), "added");
        assert!(added_diff("n.txt", "a\nb\n").contains("@@ -0,0 +1,2 @@"));
        assert!(looks_binary(&[1, 0, 2]) && !looks_binary(b"text"));
    }
}
