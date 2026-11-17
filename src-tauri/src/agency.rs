//! Agency in a box (Beyond B7): K-ONE IT SOLUTIONS takes a client's goal, quotes from
//! scorecards and history, accepts a GitHub repository invitation, runs the goal with owner
//! approval required before any merge, and delivers the PR, signed receipt, and client report.

use crate::{goals, hermes_surfaces, receipts, router, store};
use anyhow::{anyhow, bail, Result};
use serde::{Deserialize, Serialize};

const KEY_INTAKES: &str = "agency.intakes";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgencyQuote {
    pub estimated_hours: f64,
    pub confidence_pct: u8,
    pub sample_missions: usize,
    pub avg_approval_rate: f64,
    pub approved_by_owner: bool,
    pub approved_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgencyIntake {
    pub id: String,
    pub goal: String,
    pub repo: String,
    pub deadline: String,
    pub client_contact: Option<String>,
    pub created_at: i64,
    pub status: String,
    pub quote: Option<AgencyQuote>,
    pub goal_id: Option<String>,
    pub pr_url: Option<String>,
    pub report: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RepoInvitation {
    pub id: u64,
    pub repo: String,
    pub inviter: String,
    pub permissions: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgencyDelivery {
    pub intake: AgencyIntake,
    pub receipt: Option<receipts::Receipt>,
    pub report: String,
}

fn load_intakes() -> Result<Vec<AgencyIntake>> {
    let conn = store::open()?;
    let Some(raw) = store::kv_get(&conn, KEY_INTAKES)? else {
        return Ok(Vec::new());
    };
    Ok(serde_json::from_str(&raw).unwrap_or_default())
}

fn save_intakes(items: &[AgencyIntake]) -> Result<()> {
    let conn = store::open()?;
    store::kv_set(&conn, KEY_INTAKES, &serde_json::to_string(items)?)
}

pub fn list_intakes() -> Result<Vec<AgencyIntake>> {
    load_intakes()
}

/// B7.2: builds a quote purely from recorded scorecards and receipt history.
pub fn quote_from_scorecards(
    scorecards: &[router::Scorecard],
    receipts_count: usize,
    goal: &str,
) -> AgencyQuote {
    let sample_missions: usize = scorecards.iter().map(|s| s.attempts).sum();
    let avg_approval_rate = if scorecards.is_empty() {
        0.0
    } else {
        let total_rate: f64 = scorecards.iter().map(|s| s.approval_rate).sum();
        ((total_rate / scorecards.len() as f64) * 10.0).round() / 10.0
    };
    let base_hours: f64 = if goal.len() > 240 {
        12.0
    } else if goal.len() > 100 {
        6.0
    } else {
        3.0
    };
    let speed_factor: f64 = if avg_approval_rate >= 85.0 {
        0.85
    } else if avg_approval_rate >= 65.0 {
        1.0
    } else {
        1.25
    };
    let estimated_hours = ((base_hours * speed_factor) * 10.0).round() / 10.0;
    let evidence = (sample_missions + receipts_count).min(60) as f64;
    let raw_conf = if sample_missions == 0 {
        35.0
    } else {
        (avg_approval_rate * 0.65) + (evidence * 0.55)
    };
    let confidence_pct = raw_conf.round().clamp(25.0, 95.0) as u8;
    AgencyQuote {
        estimated_hours,
        confidence_pct,
        sample_missions,
        avg_approval_rate,
        approved_by_owner: false,
        approved_at: None,
    }
}

/// B7.1 + B7.2: records a client intake and computes its scorecard quote.
pub fn create_intake(
    goal: &str,
    repo: &str,
    deadline: &str,
    client_contact: Option<&str>,
) -> Result<AgencyIntake> {
    let goal = goal.trim();
    let repo = repo.trim();
    let deadline = deadline.trim();
    if goal.is_empty() || repo.is_empty() || deadline.is_empty() {
        bail!("goal, repo, and deadline are required");
    }
    let scorecards = router::scorecards().unwrap_or_default();
    let receipts_count = receipts::all().map(|r| r.len()).unwrap_or(0);
    let quote = quote_from_scorecards(&scorecards, receipts_count, goal);
    let mut items = load_intakes()?;
    let id = format!("AG-{:03}", items.len() + 1);
    let intake = AgencyIntake {
        id,
        goal: goal.to_string(),
        repo: repo.to_string(),
        deadline: deadline.to_string(),
        client_contact: client_contact
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty()),
        created_at: store::now(),
        status: "quoted".to_string(),
        quote: Some(quote),
        goal_id: None,
        pr_url: None,
        report: None,
    };
    items.push(intake.clone());
    save_intakes(&items)?;
    Ok(intake)
}

/// B7.2: sends the quote to the owner's phone via Hermes for approval.
pub fn request_quote_approval_with<F>(intake_id: &str, send_fn: F) -> Result<bool>
where
    F: FnOnce(&str) -> Result<()>,
{
    let items = load_intakes()?;
    let intake = items
        .iter()
        .find(|i| i.id == intake_id)
        .ok_or_else(|| anyhow!("intake {intake_id} not found"))?;
    let q = intake
        .quote
        .as_ref()
        .ok_or_else(|| anyhow!("intake {intake_id} has no quote"))?;
    let msg = format!(
        "Agency quote {id} for {repo} (due {due}): {hrs}h est, {conf}% confidence ({samples} missions). Approve before work starts.",
        id = intake.id,
        repo = intake.repo,
        due = intake.deadline,
        hrs = q.estimated_hours,
        conf = q.confidence_pct,
        samples = q.sample_missions
    );
    send_fn(&msg)?;
    Ok(true)
}

pub fn request_quote_approval(intake_id: &str) -> Result<bool> {
    request_quote_approval_with(intake_id, |msg| {
        let Some(target) = hermes_surfaces::notify_target() else {
            bail!("no notify target configured");
        };
        hermes_surfaces::send(&target, msg).map(|_| ())
    })
}

/// B7.2: records the owner's approval or rejection of a quote.
pub fn approve_quote(intake_id: &str, approved: bool) -> Result<AgencyIntake> {
    let mut items = load_intakes()?;
    let intake = items
        .iter_mut()
        .find(|i| i.id == intake_id)
        .ok_or_else(|| anyhow!("intake {intake_id} not found"))?;
    let Some(q) = intake.quote.as_mut() else {
        bail!("intake {intake_id} has no quote");
    };
    q.approved_by_owner = approved;
    q.approved_at = if approved { Some(store::now()) } else { None };
    intake.status = if approved { "approved" } else { "rejected" }.to_string();
    let out = intake.clone();
    save_intakes(&items)?;
    Ok(out)
}

/// B7.3: parses GitHub repository invitations JSON.
pub fn parse_invitations(json: &str) -> Result<Vec<RepoInvitation>> {
    let vals: Vec<serde_json::Value> = serde_json::from_str(json)?;
    Ok(vals
        .into_iter()
        .filter_map(|v| {
            Some(RepoInvitation {
                id: v.get("id")?.as_u64()?,
                repo: v.get("repository")?.get("full_name")?.as_str()?.to_string(),
                inviter: v
                    .get("inviter")
                    .and_then(|x| x.get("login"))
                    .and_then(|x| x.as_str())
                    .unwrap_or("unknown")
                    .to_string(),
                permissions: v
                    .get("permissions")
                    .and_then(|x| x.as_str())
                    .unwrap_or("write")
                    .to_string(),
            })
        })
        .collect())
}

/// B7.3: lists pending GitHub repository invitations via `gh api`.
pub fn list_invitations() -> Result<Vec<RepoInvitation>> {
    let gh = crate::procs::find("gh").ok_or_else(|| anyhow!("gh CLI not found"))?;
    let out = crate::procs::command(&gh)
        .args(["api", "user/repository_invitations"])
        .output()?;
    if !out.status.success() {
        bail!("gh api user/repository_invitations failed");
    }
    parse_invitations(&String::from_utf8_lossy(&out.stdout))
}

/// B7.3: accepts a GitHub repository invitation by ID.
pub fn accept_invitation(invitation_id: u64) -> Result<()> {
    let gh = crate::procs::find("gh").ok_or_else(|| anyhow!("gh CLI not found"))?;
    let endpoint = format!("user/repository_invitations/{invitation_id}");
    let out = crate::procs::command(&gh)
        .args(["api", "-X", "PATCH", &endpoint])
        .output()?;
    if !out.status.success() {
        bail!("failed to accept invitation {invitation_id}");
    }
    Ok(())
}

/// B7.3: starts a fleet goal for an approved client intake, forcing `ask` autonomy so nothing
/// on a client's repository merges without the owner's approval.
pub fn start_client_goal(intake_id: &str) -> Result<AgencyIntake> {
    let mut items = load_intakes()?;
    let intake = items
        .iter_mut()
        .find(|i| i.id == intake_id)
        .ok_or_else(|| anyhow!("intake {intake_id} not found"))?;
    let approved = intake.quote.as_ref().is_some_and(|q| q.approved_by_owner);
    if !approved {
        bail!("owner quote approval is required before starting client work");
    }
    let _ = goals::set_repo_default(&intake.repo, "ask");
    let created = goals::create(&goals::NewGoal {
        outcome: intake.goal.clone(),
        repos: vec![intake.repo.clone()],
        checks: vec![goals::Check {
            kind: "command".to_string(),
            target: "git status".to_string(),
            repo: Some(intake.repo.clone()),
        }],
        budget_usd: 0.0,
        deadline: Some(intake.deadline.clone()),
        autonomy: Some("ask".to_string()),
    })?;
    intake.goal_id = Some(created.id);
    intake.status = "active".to_string();
    let out = intake.clone();
    save_intakes(&items)?;
    Ok(out)
}

/// B7.4: formats the client delivery report with PR, signed receipt, and attribution.
pub fn build_client_report(intake: &AgencyIntake, receipt: Option<&receipts::Receipt>) -> String {
    let mut lines = vec![
        format!("K-ONE IT SOLUTIONS Delivery Report ({})", intake.id),
        format!("Repository: {}", intake.repo),
        format!("Deadline: {}", intake.deadline),
        format!("Goal: {}", intake.goal),
    ];
    if let Some(q) = &intake.quote {
        lines.push(format!(
            "Quote: {:.1}h estimated ({}% confidence across {} historical missions)",
            q.estimated_hours, q.confidence_pct, q.sample_missions
        ));
    }
    if let Some(pr) = &intake.pr_url {
        lines.push(format!("Pull Request: {pr}"));
    }
    if let Some(r) = receipt {
        lines.push(format!(
            "Signed Receipt: seq={} hash={} verdict={} verify={}",
            r.seq, r.hash, r.verdict, r.verify
        ));
        lines.push(format!("Public Key: {}", r.public_key));
        if !r.checks.is_empty() {
            let summary = r
                .checks
                .iter()
                .map(|c| format!("{}: {}", c.name, c.state))
                .collect::<Vec<_>>()
                .join(", ");
            lines.push(format!("Checks: {summary}"));
        }
    } else {
        lines.push("Signed Receipt: pending merge approval".to_string());
    }
    lines.push("Delivered by Kone Tshivhinda, K-ONE IT SOLUTIONS (AgentOS Studio).".to_string());
    lines.join("\n")
}

/// B7.4: delivers a client intake with its PR URL, matching receipt, and client report.
pub fn deliver_intake(intake_id: &str, pr_url: &str) -> Result<AgencyDelivery> {
    let pr_url = pr_url.trim();
    if pr_url.is_empty() {
        bail!("pr_url is required");
    }
    let all_receipts = receipts::all().unwrap_or_default();
    let mut items = load_intakes()?;
    let intake = items
        .iter_mut()
        .find(|i| i.id == intake_id)
        .ok_or_else(|| anyhow!("intake {intake_id} not found"))?;
    let matching_receipt = all_receipts
        .into_iter()
        .rev()
        .find(|r| r.pr_url == pr_url || r.repo == intake.repo);
    intake.pr_url = Some(pr_url.to_string());
    let report = build_client_report(intake, matching_receipt.as_ref());
    intake.report = Some(report.clone());
    intake.status = "delivered".to_string();
    let out = intake.clone();
    save_intakes(&items)?;
    Ok(AgencyDelivery {
        intake: out,
        receipt: matching_receipt,
        report,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quote_uses_scorecard_history_and_confidence() {
        let cards = vec![router::Scorecard {
            role: "implementer".into(),
            provider: "anthropic".into(),
            model: "claude-sonnet".into(),
            attempts: 20,
            approvals: 18,
            approval_rate: 90.0,
            avg_cost_usd: 0.25,
            score: 0.9,
            eval_total: 5,
            eval_improving: 4,
            ready_for_default: true,
            reason: "ready".into(),
        }];
        let q = quote_from_scorecards(&cards, 15, "Implement OAuth login");
        assert_eq!(q.sample_missions, 20);
        assert_eq!(q.avg_approval_rate, 90.0);
        assert!(q.confidence_pct >= 70);
        assert!(!q.approved_by_owner);
    }

    #[test]
    fn parse_invitations_reads_github_payload() {
        let raw = r#"[{"id":42,"permissions":"write","repository":{"full_name":"client/app"},"inviter":{"login":"alice"}}]"#;
        let list = parse_invitations(raw).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, 42);
        assert_eq!(list[0].repo, "client/app");
        assert_eq!(list[0].inviter, "alice");
    }

    #[test]
    fn client_goal_requires_owner_approval_and_forces_ask_autonomy() {
        let d = tempfile::TempDir::new().unwrap();
        store::set_test_db(Some(d.path().join("s.db")));
        let intake = create_intake("Add SSO", "client/sso-app", "2026-12-01", None).unwrap();
        assert!(start_client_goal(&intake.id).is_err());
        let approved = approve_quote(&intake.id, true).unwrap();
        assert!(approved.quote.unwrap().approved_by_owner);
        let active = start_client_goal(&intake.id).unwrap();
        assert_eq!(active.status, "active");
        let goal_id = active.goal_id.as_deref().unwrap();
        let g = goals::list(50)
            .unwrap()
            .into_iter()
            .find(|x| x.id == goal_id)
            .unwrap();
        assert_eq!(g.autonomy, "ask");
        store::set_test_db(None);
    }

    #[test]
    fn delivery_report_includes_pr_receipt_and_kone_attribution() {
        let d = tempfile::TempDir::new().unwrap();
        store::set_test_db(Some(d.path().join("s.db")));
        let intake = create_intake("Fix billing", "client/billing", "2026-12-01", None).unwrap();
        let del = deliver_intake(&intake.id, "https://github.com/client/billing/pull/9").unwrap();
        assert_eq!(del.intake.status, "delivered");
        assert!(del
            .report
            .contains("https://github.com/client/billing/pull/9"));
        assert!(del.report.contains("Kone Tshivhinda, K-ONE IT SOLUTIONS"));
        store::set_test_db(None);
    }
}
