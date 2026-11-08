//! Live Brain view check: one memory node is deleted on a throwaway profile, never the default one.
//! `cargo test --test live_brain -- --ignored --nocapture --test-threads 1`
//!
//! Hold the live-machine lock first (`04_Agent_Engine/locks` in the vault) and delete it after.

use std::path::PathBuf;

use agentos_studio_lib::{hermes_surfaces, permissions, procs};

fn memory_files() -> (PathBuf, PathBuf) {
    let dir = procs::hermes_home().join("memories");
    (dir.join("MEMORY.md"), dir.join("USER.md"))
}

fn bytes(p: &PathBuf) -> Option<Vec<u8>> {
    std::fs::read(p).ok()
}

/// Removes the profile even when an assertion fails halfway through.
struct ThrowawayProfile(String);

impl Drop for ThrowawayProfile {
    fn drop(&mut self) {
        let _ = procs::run("hermes", &["profile", "delete", &self.0, "-y"]);
    }
}

fn hermes_ok(args: &[&str]) -> String {
    let out = procs::run("hermes", args).expect("hermes is installed");
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

#[test]
#[ignore = "creates and deletes a real Hermes profile"]
fn brain_view_reads_and_deletes_a_node_on_a_throwaway_profile() {
    let (default_memory, default_user) = memory_files();
    let before = (bytes(&default_memory), bytes(&default_user));

    let name = format!(
        "e1-brain-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    );
    assert!(
        !hermes_surfaces::profile_home(&name).exists(),
        "refusing to overwrite an existing profile"
    );
    hermes_ok(&["profile", "create", &name, "--no-skills", "--no-alias"]);
    let profile = ThrowawayProfile(name.clone());

    let memories = hermes_surfaces::profile_home(&name).join("memories");
    std::fs::create_dir_all(&memories).unwrap();
    std::fs::write(
        memories.join("MEMORY.md"),
        "Brain view live test entry\n\n§\n\nA second live test entry\n",
    )
    .unwrap();

    let view = hermes_surfaces::memory_view_with(Some(&name));
    assert!(view.memory_present, "MEMORY.md should be readable");
    assert!(
        view.memory_modified.is_some(),
        "the view should report when the file last changed"
    );
    if let Some(e) = &view.journey_error {
        panic!("hermes journey failed on the throwaway profile: {e}");
    }
    let ids: Vec<String> = view.journey["nodes"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter(|n| n["kind"] == "memory")
                .filter_map(|n| n["id"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    assert!(
        !ids.is_empty(),
        "the seeded memory should appear in the journey graph: {}",
        view.journey
    );
    let node = ids[0].clone();

    permissions::run_guarded(
        "hermes.journey.delete",
        &node,
        "live_brain: delete a memory node on a throwaway profile",
        true,
        || hermes_surfaces::journey_delete_with(Some(&name), &node),
    )
    .expect("the permission path should delete the node");

    let after = hermes_surfaces::memory_view_with(Some(&name));
    let still_there = after.journey["nodes"]
        .as_array()
        .map(|a| a.iter().any(|n| n["id"] == node.as_str()))
        .unwrap_or(false);
    assert!(
        !still_there,
        "the deleted node is still in the journey graph"
    );

    drop(profile);
    assert!(
        !hermes_surfaces::profile_home(&name).exists(),
        "the throwaway profile should be gone"
    );
    assert_eq!(
        (bytes(&default_memory), bytes(&default_user)),
        before,
        "the default profile's MEMORY.md and USER.md must be byte-identical"
    );
}
