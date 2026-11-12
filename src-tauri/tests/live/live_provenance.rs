//! Live provenance check: a real git blame on a temporary repository. A commit whose message
//! carries a mission's trailers resolves back to the mission; a later commit without trailers
//! resolves to no mission; an uncommitted line is an explicit error. The mission row is seeded in
//! the real studio.db and deleted on drop.
//! `cargo test --test live -- live_provenance:: --ignored --nocapture --test-threads 1`
//!
//! Run only under the live-machine lock (`04_Agent_Engine/locks` in the vault). It never launches
//! Hermes: git and the store only.

use std::path::Path;
use std::process::Command;

use agentos_studio_lib::{missions, provenance, store};

/// Holds the automation loop off while the test runs: the running app would otherwise start the
/// seeded queued mission in its real repository. The previous value is put back on drop.
struct AutomationOff(Option<String>);

impl AutomationOff {
    fn new() -> AutomationOff {
        let c = store::open().unwrap();
        let before = store::kv_get(&c, "automation.enabled").unwrap();
        store::kv_set(&c, "automation.enabled", "0").unwrap();
        AutomationOff(before)
    }
}

impl Drop for AutomationOff {
    fn drop(&mut self) {
        let c = store::open().unwrap();
        match &self.0 {
            Some(v) => {
                let _ = store::kv_set(&c, "automation.enabled", v);
            }
            None => {
                let _ = store::kv_delete(&c, "automation.enabled");
            }
        }
    }
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=Qoder",
            "-c",
            "user.email=qoder@example.invalid",
        ])
        .args(["-c", "commit.gpgsign=false"])
        .args(args)
        .output()
        .expect("git is installed");
    assert!(
        out.status.success(),
        "git {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn write(dir: &Path, name: &str, text: &str) {
    let path = dir.join(name);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/** Delete the seeded mission even when an assertion panics. */
struct Seeded(String);

impl Drop for Seeded {
    fn drop(&mut self) {
        let _ = missions::delete(&self.0);
    }
}

#[test]
#[ignore = "needs git and the owner's real store; run it on the live machine under the lock"]
fn a_trailered_commit_names_its_mission_and_a_plain_one_does_not() {
    let _off = AutomationOff::new();
    let m = missions::create(
        "agentos-studio",
        "Prove the provenance line",
        "A blamed line names the mission that wrote it",
        "implementer",
        "qoder",
        "",
        "",
        0.0,
    )
    .unwrap();
    let _seeded = Seeded(m.id.clone());

    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "-q"]);

    write(dir.path(), "src/lib.rs", "one\ntwo\n");
    git(dir.path(), &["add", "-A"]);
    let message = format!("Write the first two lines\n\n{}", provenance::trailers(&m));
    git(dir.path(), &["commit", "-q", "-m", &message]);

    let got = provenance::why(&dir.path().to_string_lossy(), "src/lib.rs", 1).unwrap();
    assert_eq!(got.source, "trailers");
    assert_eq!(got.file, "src/lib.rs");
    assert_eq!(got.line, 1);
    assert_eq!(got.subject, "Write the first two lines");
    let found = got.mission.expect("the trailers name the mission");
    assert_eq!(found.id, m.id);
    assert_eq!(found.goal, m.goal);
    assert_eq!(found.agent, "qoder");

    write(dir.path(), "src/lib.rs", "one\ntwo\nthree\n");
    git(dir.path(), &["add", "-A"]);
    git(dir.path(), &["commit", "-q", "-m", "A human edit"]);

    let plain = provenance::why(&dir.path().to_string_lossy(), "src/lib.rs", 3).unwrap();
    assert_eq!(plain.source, "none");
    assert!(plain.mission.is_none());
    assert_eq!(plain.subject, "A human edit");
    assert!(
        plain.note.is_some(),
        "a temporary repository has no GitHub remote, so the lookup should say why it was skipped"
    );

    write(dir.path(), "src/lib.rs", "one\ntwo\nthree\nfour\n");
    let err = provenance::why(&dir.path().to_string_lossy(), "src/lib.rs", 4)
        .unwrap_err()
        .to_string();
    assert!(err.contains("not committed yet"), "{err}");
}
