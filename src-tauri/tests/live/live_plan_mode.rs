//! Plan mode against the real Hermes install: a scoped `/plan` turn in a scratch repository must
//! write a plan Studio can find and leave every other file untouched.
//! Run: cargo test --test live -- live_plan_mode:: --ignored --nocapture
use agentos_studio_lib::plans;
use std::process::Command;

#[test]
#[ignore]
fn plan_turn_writes_only_a_plan() {
    let dir = std::env::temp_dir().join(format!("plan-live-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("README.md"), "# demo\n").unwrap();
    let since = chrono::Utc::now().timestamp() - 2;
    let prompt = plans::plan_prompt("Add a CONTRIBUTING.md with a how-to-run section.", &dir);
    let qf = dir
        .join("..")
        .join(format!("plan-q-{}.md", std::process::id()));
    std::fs::write(&qf, &prompt).unwrap();
    let out = Command::new("hermes")
        .args([
            "chat",
            "--query-file",
            &qf.display().to_string(),
            "--source",
            "gui",
            "--format",
            "stream-json",
        ])
        .args([
            "--provider",
            "claude-subscription-directsdk-experimental",
            "-m",
            "claude-haiku-5-5[1m]",
        ])
        .args(["--in", &dir.display().to_string()])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let sid = stdout
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .find_map(|v| {
            v.get("session_id")
                .and_then(|s| s.as_str())
                .filter(|s| !s.is_empty())
                .map(String::from)
        });
    let plan = plans::latest_since(&dir, since).unwrap();
    let mut files: Vec<String> = walk(&dir);
    files.sort();
    println!("plan: {:?}", plan.as_ref().map(|p| (&p.path, p.body.len())));
    println!("files: {files:?}");
    if let Some(s) = sid {
        let _ = Command::new("hermes")
            .args(["sessions", "delete", &s, "--yes"])
            .output();
    }
    let _ = std::fs::remove_file(&qf);
    let _ = std::fs::remove_dir_all(&dir);
    let plan = plan.expect("no plan file written");
    assert!(plan.body.len() > 50);
    assert!(
        files
            .iter()
            .all(|f| f == "README.md" || f.starts_with(".hermes/plans/")),
        "a planning turn changed other files: {files:?}"
    );
}

fn walk(root: &std::path::Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else {
                out.push(
                    p.strip_prefix(root)
                        .unwrap()
                        .display()
                        .to_string()
                        .replace('\\', "/"),
                );
            }
        }
    }
    out
}
