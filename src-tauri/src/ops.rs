//! v0.3 Tauri commands: terminals, updates, workspace organisation, GitHub and safety.

use serde_json::Value;
use tauri::{AppHandle, State};

use crate::commands::AppState;
use crate::org::{self, FolderPatch, OrgSnapshot};
use crate::permissions::{self, AuditEntry, PolicyView};
use crate::pty::{self, PtyInfo, SpawnRequest};
use crate::updates::{self, BackupManifest, UpdateReport};
use crate::{github, hermes_engine, store};

type CmdResult<T> = Result<T, String>;

fn err(e: anyhow::Error) -> String {
    format!("{e:#}")
}

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> anyhow::Result<T> + Send + 'static,
) -> CmdResult<T> {
    tauri::async_runtime::spawn_blocking(f)
        .await
        .map_err(|e| e.to_string())?
        .map_err(err)
}

// ------------------------------------------------------------------ terminals

#[tauri::command]
pub fn pty_spawn(
    app: AppHandle,
    state: State<'_, AppState>,
    request: SpawnRequest,
) -> CmdResult<PtyInfo> {
    let target = format!(
        "{} in {}",
        request.profile,
        request.cwd.clone().unwrap_or_default()
    );
    let out = pty::spawn(app, &state.pty, request).map_err(err);
    permissions::audit(
        "terminal.open",
        &target,
        "",
        if out.is_ok() { "ok" } else { "error" },
    );
    out
}

#[tauri::command]
pub fn pty_write(state: State<'_, AppState>, id: String, data: String) -> CmdResult<()> {
    pty::write(&state.pty, &id, &data).map_err(err)
}

#[tauri::command]
pub fn pty_resize(state: State<'_, AppState>, id: String, cols: u16, rows: u16) -> CmdResult<()> {
    pty::resize(&state.pty, &id, cols, rows).map_err(err)
}

#[tauri::command]
pub fn pty_kill(state: State<'_, AppState>, id: String) -> CmdResult<()> {
    pty::kill(&state.pty, &id).map_err(err)
}

#[tauri::command]
pub fn pty_list(state: State<'_, AppState>) -> Vec<PtyInfo> {
    pty::list(&state.pty)
}

#[tauri::command]
pub fn pty_snapshot(state: State<'_, AppState>, id: String) -> CmdResult<pty::Snapshot> {
    pty::snapshot(&state.pty, &id).map_err(err)
}

// ------------------------------------------------------------------ updates

#[tauri::command]
pub async fn check_updates() -> CmdResult<UpdateReport> {
    blocking(|| Ok(updates::check_all())).await
}

#[tauri::command]
pub async fn last_update_report() -> CmdResult<Option<Value>> {
    blocking(|| {
        let c = store::open()?;
        Ok(store::kv_get(&c, "update.last_report")?.and_then(|s| serde_json::from_str(&s).ok()))
    })
    .await
}

#[tauri::command]
pub async fn apply_update(
    app: AppHandle,
    product: String,
    approved: Option<bool>,
) -> CmdResult<String> {
    blocking(move || updates::apply_update(app, &product, approved.unwrap_or(false))).await
}

#[tauri::command]
pub async fn skip_update(product: String, version: String) -> CmdResult<()> {
    blocking(move || updates::skip_version(&product, &version)).await
}

#[tauri::command]
pub async fn snooze_update(product: String, hours: i64) -> CmdResult<i64> {
    blocking(move || updates::snooze(&product, hours)).await
}

#[tauri::command]
pub async fn clear_update_choice(product: String) -> CmdResult<()> {
    blocking(move || updates::clear_skip(&product)).await
}

#[tauri::command]
pub async fn list_update_backups() -> CmdResult<Vec<BackupManifest>> {
    blocking(|| Ok(updates::list_backups())).await
}

#[tauri::command]
pub async fn rollback_update(
    app: AppHandle,
    backup_id: String,
    approved: Option<bool>,
) -> CmdResult<String> {
    blocking(move || updates::rollback(app, &backup_id, approved.unwrap_or(false))).await
}

// ------------------------------------------------------------------ organisation

#[tauri::command]
pub async fn org_snapshot(query: Option<String>) -> CmdResult<OrgSnapshot> {
    blocking(move || {
        let sessions = hermes_engine::list_hermes_sessions(
            query.as_deref().filter(|q| !q.trim().is_empty()),
            500,
        )
        .unwrap_or_default();
        let c = store::open()?;
        org::snapshot_in(&c, sessions, org::hermes_projects())
    })
    .await
}

#[tauri::command]
pub async fn org_create_workspace(name: String) -> CmdResult<org::Workspace> {
    blocking(move || org::create_workspace(&store::open()?, &name)).await
}

#[tauri::command]
pub async fn org_update_workspace(
    id: String,
    name: Option<String>,
    archived: Option<bool>,
    color: Option<String>,
) -> CmdResult<()> {
    blocking(move || {
        org::update_workspace(
            &store::open()?,
            &id,
            name.as_deref(),
            archived,
            color.as_deref(),
        )
    })
    .await
}

#[tauri::command]
pub async fn org_delete_workspace(id: String, approved: Option<bool>) -> CmdResult<()> {
    blocking(move || {
        let c = store::open()?;
        let name: String = c
            .query_row("SELECT name FROM workspaces WHERE id = ?1", [&id], |r| {
                r.get(0)
            })
            .map_err(|_| anyhow::anyhow!("Workspace not found"))?;
        permissions::run_guarded(
            "org.delete",
            &id,
            &format!(
                "Delete workspace \"{name}\" and its folders. Chats are kept and become Unfiled."
            ),
            approved.unwrap_or(false),
            || org::delete_workspace(&c, &id),
        )
    })
    .await
}

#[tauri::command]
pub async fn org_create_folder(
    workspace_id: String,
    parent_id: Option<String>,
    name: String,
    path: Option<String>,
) -> CmdResult<String> {
    blocking(move || {
        org::create_folder(
            &store::open()?,
            &workspace_id,
            parent_id.as_deref(),
            &name,
            path.as_deref(),
        )
    })
    .await
}

#[tauri::command]
pub async fn org_update_folder(id: String, patch: FolderPatch) -> CmdResult<()> {
    blocking(move || org::update_folder(&store::open()?, &id, &patch)).await
}

#[tauri::command]
pub async fn org_delete_folder(id: String, approved: Option<bool>) -> CmdResult<()> {
    blocking(move || {
        let c = store::open()?;
        let name: String = c
            .query_row("SELECT name FROM folders WHERE id = ?1", [&id], |r| r.get(0))
            .map_err(|_| anyhow::anyhow!("Folder not found"))?;
        permissions::run_guarded(
            "org.delete",
            &id,
            &format!("Delete folder \"{name}\" and its sub-folders. Chats are kept and return to automatic placement."),
            approved.unwrap_or(false),
            || org::delete_folder(&c, &id),
        )
    })
    .await
}

#[tauri::command]
pub async fn org_place_chat(
    session_id: String,
    folder_id: Option<String>,
    workspace_id: Option<String>,
) -> CmdResult<()> {
    blocking(move || {
        org::place_chat(
            &store::open()?,
            &session_id,
            folder_id.as_deref(),
            workspace_id.as_deref(),
        )
    })
    .await
}

#[tauri::command]
pub async fn org_set_chat_flags(
    session_id: String,
    pinned: Option<bool>,
    archived: Option<bool>,
) -> CmdResult<()> {
    blocking(move || org::set_chat_flags(&store::open()?, &session_id, pinned, archived)).await
}

#[tauri::command]
pub async fn org_set_tags(
    kind: String,
    item_id: String,
    tags: Vec<String>,
) -> CmdResult<Vec<String>> {
    blocking(move || org::set_tags(&store::open()?, &kind, &item_id, &tags)).await
}

#[tauri::command]
pub async fn org_add_attachment(
    kind: String,
    owner_id: String,
    path: String,
    label: Option<String>,
) -> CmdResult<String> {
    blocking(move || {
        org::add_attachment(&store::open()?, &kind, &owner_id, &path, label.as_deref())
    })
    .await
}

#[tauri::command]
pub async fn org_remove_attachment(id: String) -> CmdResult<()> {
    blocking(move || org::remove_attachment(&store::open()?, &id)).await
}

#[tauri::command]
pub async fn org_context_files(
    session_id: Option<String>,
    folder_id: Option<String>,
) -> CmdResult<Vec<String>> {
    blocking(move || {
        org::context_files(&store::open()?, session_id.as_deref(), folder_id.as_deref())
    })
    .await
}

#[tauri::command]
pub async fn org_link_project(folder_id: String, approved: Option<bool>) -> CmdResult<String> {
    blocking(move || org::link_project(&folder_id, approved.unwrap_or(false))).await
}

// ------------------------------------------------------------------ GitHub

#[tauri::command]
pub async fn gh_auth_status() -> CmdResult<Value> {
    blocking(github::auth_status).await
}

#[tauri::command]
pub async fn gh_login() -> CmdResult<String> {
    blocking(github::login).await
}

#[tauri::command]
pub async fn gh_repos(limit: Option<u32>) -> CmdResult<Value> {
    blocking(move || github::repos(limit.unwrap_or(100))).await
}

#[tauri::command]
pub async fn gh_repo_for_dir(dir: String) -> CmdResult<Value> {
    blocking(move || github::repo_for_dir(&dir)).await
}

#[tauri::command]
pub async fn gh_branches(repo: String) -> CmdResult<Value> {
    blocking(move || github::branches(&repo)).await
}

#[tauri::command]
pub async fn gh_commits(repo: String, branch: Option<String>) -> CmdResult<Value> {
    blocking(move || github::commits(&repo, branch.as_deref())).await
}

#[tauri::command]
pub async fn gh_prs(repo: String, state: String) -> CmdResult<Value> {
    blocking(move || github::prs(&repo, &state)).await
}

#[tauri::command]
pub async fn gh_pr_detail(repo: String, number: u64) -> CmdResult<Value> {
    blocking(move || github::pr_detail(&repo, number)).await
}

#[tauri::command]
pub async fn gh_pr_diff(repo: String, number: u64) -> CmdResult<String> {
    blocking(move || github::pr_diff(&repo, number)).await
}

#[tauri::command]
pub async fn gh_pr_merge(
    repo: String,
    number: u64,
    method: String,
    delete_branch: bool,
    approved: Option<bool>,
) -> CmdResult<String> {
    blocking(move || {
        github::pr_merge(
            &repo,
            number,
            &method,
            delete_branch,
            approved.unwrap_or(false),
        )
    })
    .await
}

#[tauri::command]
pub async fn gh_pr_review(
    repo: String,
    number: u64,
    event: String,
    body: String,
    approved: Option<bool>,
) -> CmdResult<String> {
    blocking(move || github::pr_review(&repo, number, &event, &body, approved.unwrap_or(false)))
        .await
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn gh_pr_create(
    dir: String,
    title: String,
    body: String,
    base: Option<String>,
    head: Option<String>,
    draft: bool,
    approved: Option<bool>,
) -> CmdResult<String> {
    blocking(move || {
        github::pr_create(
            &dir,
            &title,
            &body,
            base.as_deref(),
            head.as_deref(),
            draft,
            approved.unwrap_or(false),
        )
    })
    .await
}

#[tauri::command]
pub async fn gh_issues(repo: String, state: String) -> CmdResult<Value> {
    blocking(move || github::issues(&repo, &state)).await
}

#[tauri::command]
pub async fn gh_issue_detail(repo: String, number: u64) -> CmdResult<Value> {
    blocking(move || github::issue_detail(&repo, number)).await
}

#[tauri::command]
pub async fn gh_issue_create(
    repo: String,
    title: String,
    body: String,
    approved: Option<bool>,
) -> CmdResult<String> {
    blocking(move || github::issue_create(&repo, &title, &body, approved.unwrap_or(false))).await
}

#[tauri::command]
pub async fn gh_comment(
    repo: String,
    kind: String,
    number: u64,
    body: String,
    approved: Option<bool>,
) -> CmdResult<String> {
    blocking(move || github::comment(&repo, &kind, number, &body, approved.unwrap_or(false))).await
}

#[tauri::command]
pub async fn gh_issue_close(
    repo: String,
    number: u64,
    reason: String,
    approved: Option<bool>,
) -> CmdResult<String> {
    blocking(move || github::issue_close(&repo, number, &reason, approved.unwrap_or(false))).await
}

#[tauri::command]
pub async fn gh_runs(repo: String, branch: Option<String>) -> CmdResult<Value> {
    blocking(move || github::runs(&repo, branch.as_deref())).await
}

#[tauri::command]
pub async fn gh_run_detail(repo: String, id: u64) -> CmdResult<Value> {
    blocking(move || github::run_detail(&repo, id)).await
}

#[tauri::command]
pub async fn gh_run_failed_log(repo: String, id: u64) -> CmdResult<String> {
    blocking(move || github::run_failed_log(&repo, id)).await
}

#[tauri::command]
pub async fn gh_run_rerun(
    repo: String,
    id: u64,
    failed_only: bool,
    approved: Option<bool>,
) -> CmdResult<String> {
    blocking(move || github::run_rerun(&repo, id, failed_only, approved.unwrap_or(false))).await
}

// ------------------------------------------------------------------ OS helpers

/// Only plain https links leave the app (no other schemes, quotes or whitespace).
pub fn external_url_ok(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://") else {
        return false;
    };
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    !url.chars()
        .any(|c| c.is_whitespace() || c == '"' || c == '\'' || c == '`')
        && !host.is_empty()
        && host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':'))
}

fn open_with_os(target: &str) -> anyhow::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        std::process::Command::new("rundll32")
            .args(["url.dll,FileProtocolHandler", target])
            .creation_flags(crate::procs::CREATE_NO_WINDOW)
            .spawn()?;
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open").arg(target).spawn()?;
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        std::process::Command::new("xdg-open").arg(target).spawn()?;
    }
    Ok(())
}

#[tauri::command]
pub fn open_external(url: String) -> CmdResult<()> {
    if !external_url_ok(&url) {
        return Err(format!("Refusing to open {url}"));
    }
    open_with_os(&url).map_err(err)
}

/// Show a file or folder in the system file manager.
#[tauri::command]
pub fn reveal_path(path: String) -> CmdResult<()> {
    let p = std::path::Path::new(&path);
    if !p.exists() {
        return Err(format!("{path} does not exist"));
    }
    let dir = if p.is_dir() {
        p
    } else {
        p.parent().unwrap_or(p)
    };
    open_with_os(&dir.display().to_string()).map_err(err)
}

// ------------------------------------------------------------------ safety

#[tauri::command]
pub async fn list_policies() -> CmdResult<Vec<PolicyView>> {
    blocking(|| permissions::list_policies_in(&store::open()?)).await
}

#[tauri::command]
pub async fn set_policy(action: String, policy: String) -> CmdResult<()> {
    blocking(move || permissions::set_policy_in(&store::open()?, &action, &policy)).await
}

#[tauri::command]
pub async fn list_audit(limit: Option<usize>, query: Option<String>) -> CmdResult<Vec<AuditEntry>> {
    blocking(move || {
        permissions::list_audit_in(
            &store::open()?,
            limit.unwrap_or(300).min(5000),
            query.as_deref(),
        )
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::external_url_ok;

    #[test]
    fn only_plain_https_links_open() {
        assert!(external_url_ok("https://github.com/o/r/pull/1"));
        assert!(external_url_ok(
            "https://github.com/anthropics/claude-code/blob/main/CHANGELOG.md#x"
        ));
        assert!(!external_url_ok("http://example.com"));
        assert!(!external_url_ok("file:///C:/Windows"));
        assert!(!external_url_ok("https://a.com/\" & calc"));
        assert!(!external_url_ok("https://"));
        assert!(!external_url_ok("javascript:alert(1)"));
    }
}
