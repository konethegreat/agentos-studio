//! Live memory quality check: the weekly job is created on a throwaway Hermes profile, the built
//! exe runs the check against the real vault in `--dry-run`, every vault file hash is unchanged,
//! and the job is removed again.
//! `cargo test --test live_memory_quality -- --ignored --nocapture --test-threads 1`
//!
//! Run only when the owner has allowed Hermes use. Hold the live-machine lock first
//! (`04_Agent_Engine/locks` in the vault) and delete it after. The cron job and profile live on a
//! throwaway profile and are deleted afterwards; the dry run writes nothing into the vault.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use agentos_studio_lib::memory_quality::{weekly_script, WEEKLY_JOB, WEEKLY_SCHEDULE};

fn home() -> PathBuf {
    agentos_studio_lib::procs::hermes_home()
}

fn hermes(args: &[&str]) -> String {
    let out = Command::new("hermes")
        .args(args)
        .output()
        .expect("hermes is installed");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out.status.success(),
        "hermes {} failed: {text}",
        args.join(" ")
    );
    text
}

struct ThrowawayProfile(String);

impl Drop for ThrowawayProfile {
    fn drop(&mut self) {
        // The host gateway keeps the profile's state.db open while it serves it. Unserve first,
        // then retry the delete: the gateway rescans within 30 seconds.
        let _ = Command::new("hermes")
            .args(["-p", &self.0, "gateway", "stop"])
            .output();
        for _ in 0..6 {
            let _ = Command::new("hermes")
                .args(["profile", "delete", &self.0, "-y"])
                .output();
            if !home().join("profiles").join(&self.0).exists() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_secs(5));
        }
    }
}

fn fnv(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    h
}

/// Every Markdown file in the vault, by relative path. The dry run must leave all of them alone.
fn vault_hashes(root: &Path) -> BTreeMap<String, u64> {
    walkdir::WalkDir::new(root)
        .into_iter()
        .flatten()
        .filter(|e| e.file_type().is_file())
        .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("md"))
        .filter_map(|e| {
            let rel = e
                .path()
                .strip_prefix(root)
                .ok()?
                .to_string_lossy()
                .replace('\\', "/");
            let bytes = std::fs::read(e.path()).ok()?;
            Some((rel, fnv(&bytes)))
        })
        .collect()
}

#[test]
#[ignore = "needs the live machine, Hermes, and the owner's permission"]
fn the_weekly_job_runs_the_check_and_a_dry_run_changes_nothing() {
    let vault = agentos_studio_lib::settings::vault_path();
    assert!(vault.is_dir(), "no vault at {}", vault.display());
    let before = vault_hashes(&vault);
    assert!(!before.is_empty(), "the vault has no notes to check");

    let name = format!("e3-quality-{}", std::process::id());
    let profile = ThrowawayProfile(name.clone());
    hermes(&["profile", "create", &name, "--no-alias"]);

    let scripts = home().join("profiles").join(&name).join("scripts");
    std::fs::create_dir_all(&scripts).expect("create the profile scripts folder");
    let script = scripts.join(format!("{WEEKLY_JOB}.py"));
    let exe = PathBuf::from(env!("CARGO_BIN_EXE_agentos-studio"));
    std::fs::write(&script, weekly_script(&exe, &vault)).expect("write the weekly script");
    // Hermes resolves the script inside the profile's scripts folder and rejects an absolute path.
    let script_name = format!("{WEEKLY_JOB}.py");

    let created = hermes(&[
        "-p",
        &name,
        "cron",
        "create",
        WEEKLY_SCHEDULE,
        "--name",
        WEEKLY_JOB,
        "--no-agent",
        "--script",
        &script_name,
    ]);
    let id = created
        .lines()
        .find_map(|l| l.strip_prefix("Created job: "))
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .expect("cron create prints the job id")
        .to_string();
    let listed = hermes(&["-p", &name, "cron", "list"]);
    assert!(
        listed.contains(&id),
        "cron list does not show {id}: {listed}"
    );
    assert!(
        listed.contains(WEEKLY_JOB),
        "cron list has no job name: {listed}"
    );

    let out = Command::new(&exe)
        .args([
            "--memory-quality",
            "--vault",
            &vault.display().to_string(),
            "--dry-run",
        ])
        .output()
        .expect("run the built exe");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "the dry run failed: {stdout}{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(stdout.contains("Memory quality dry run"), "{stdout}");

    let after = vault_hashes(&vault);
    assert_eq!(before, after, "a dry run must not change a vault file");

    hermes(&["-p", &name, "cron", "remove", &id]);
    let listed = hermes(&["-p", &name, "cron", "list"]);
    assert!(!listed.contains(&id), "the job is still listed: {listed}");

    drop(profile);
}
