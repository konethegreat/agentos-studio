//! Daily fleet health report (S5): sends one morning WhatsApp digest with open/closed
//! lanes, repairs made, stalled missions and why, and repositories that moved.

use chrono::{Local, Timelike};
use serde::Serialize;
use std::sync::{Mutex, OnceLock};

use crate::audit;
use crate::autonomy;
use crate::fleet_doctor::{self, DoctorVerdict};
use crate::missions::{self, Mission};
use crate::quota_grid::{self, Lane};
use crate::whatsapp;

const MORNING_HOUR: u32 = 7;

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct DailyFleetReport {
    pub date: String,
    pub sent_at: Option<i64>,
    pub open_lanes: usize,
    pub closed_lanes: usize,
    pub repairs: Vec<String>,
    pub stalled_missions: Vec<String>,
    pub moved_repos: Vec<String>,
    pub message: String,
}

static LAST_REPORT: OnceLock<Mutex<Option<DailyFleetReport>>> = OnceLock::new();

fn report_lock() -> &'static Mutex<Option<DailyFleetReport>> {
    LAST_REPORT.get_or_init(|| Mutex::new(None))
}

pub fn status() -> Option<DailyFleetReport> {
    report_lock().lock().unwrap().clone()
}

pub fn step() {
    if crate::pause::paused() {
        return;
    }
    let local = Local::now();
    if local.hour() < MORNING_HOUR {
        return;
    }
    let today = local.format("%Y-%m-%d").to_string();
    if let Some(last) = report_lock().lock().unwrap().as_ref() {
        if last.date == today && last.sent_at.is_some() {
            return;
        }
    }
    let _ = send_now();
}

pub fn send_now() -> DailyFleetReport {
    let today = Local::now().format("%Y-%m-%d").to_string();
    let now = chrono::Utc::now().timestamp();
    let lanes = quota_grid::lanes(false);
    let doc = fleet_doctor::status();
    let verdicts = doc.map(|d| d.verdicts).unwrap_or_default();
    let missions_all = missions::list(None).unwrap_or_default();
    let repos = autonomy::list().unwrap_or_default();
    let repairs = crate::fallback_chain::status()
        .entries
        .into_iter()
        .filter(|e| e.demoted)
        .map(|e| format!("demoted {} ({})", e.model, e.provider))
        .collect::<Vec<_>>();

    let rep = build_and_send(&today, now, &lanes, &repairs, &verdicts, &missions_all, &repos, |msg| {
        whatsapp::notify(msg);
    });
    *report_lock().lock().unwrap() = Some(rep.clone());
    rep
}

pub fn build_report(
    date: &str,
    lanes: &[Lane],
    repairs: &[String],
    verdicts: &[DoctorVerdict],
    missions: &[Mission],
    repos: &[autonomy::RepoRow],
    now_ts: i64,
) -> DailyFleetReport {
    let open_lanes = lanes.iter().filter(|l| l.open).count();
    let closed_lanes = lanes.len().saturating_sub(open_lanes);
    let stalled_missions: Vec<String> = verdicts
        .iter()
        .map(|v| format!("{} ({}h): {}", v.mission_id, v.stalled_hours, v.waiting_on))
        .collect();
    let cutoff = now_ts - 24 * 3600;
    let mut moved_repos: Vec<String> = missions
        .iter()
        .filter(|m| m.status == "done" && m.updated_at >= cutoff)
        .map(|m| m.repo.clone())
        .collect();
    for r in repos {
        if r.active_goal.is_some() && !moved_repos.contains(&r.repo) {
            moved_repos.push(r.repo.clone());
        }
    }
    moved_repos.sort();
    moved_repos.dedup();

    let repair_text = if repairs.is_empty() {
        "none".to_string()
    } else {
        repairs.join(", ")
    };
    let stalled_text = if stalled_missions.is_empty() {
        "none".to_string()
    } else {
        stalled_missions.join("; ")
    };
    let moved_text = if moved_repos.is_empty() {
        "none".to_string()
    } else {
        moved_repos.join(", ")
    };

    let message = format!(
        "AgentOS Fleet ({date}):\nLanes: {open_lanes} open, {closed_lanes} closed\nRepairs: {repair_text}\nStalled: {stalled_text}\nMoved repos: {moved_text}"
    );

    DailyFleetReport {
        date: date.to_string(),
        sent_at: None,
        open_lanes,
        closed_lanes,
        repairs: repairs.to_vec(),
        stalled_missions,
        moved_repos,
        message,
    }
}

pub fn build_and_send<F>(
    date: &str,
    now_ts: i64,
    lanes: &[Lane],
    repairs: &[String],
    verdicts: &[DoctorVerdict],
    missions: &[Mission],
    repos: &[autonomy::RepoRow],
    send_fn: F,
) -> DailyFleetReport
where
    F: FnOnce(&str),
{
    let mut rep = build_report(date, lanes, repairs, verdicts, missions, repos, now_ts);
    send_fn(&rep.message);
    rep.sent_at = Some(now_ts);
    let _ = audit::record("fleet_report", &format!("sent daily report for {date}"));
    rep
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_and_sends_daily_fleet_digest() {
        let lanes = vec![
            Lane {
                provider: "claude".into(),
                model: "claude-sonnet-5-5".into(),
                primary: true,
                headroom_pct: Some(100.0),
                resets_at: None,
                exhausted_until: None,
                open: true,
                why: "ready".into(),
            },
            Lane {
                provider: "openrouter".into(),
                model: "gpt-5".into(),
                primary: false,
                headroom_pct: Some(0.0),
                resets_at: None,
                exhausted_until: None,
                open: false,
                why: "exhausted".into(),
            },
        ];
        let verdicts = vec![DoctorVerdict {
            mission_id: "M-12".into(),
            repo: "app".into(),
            title: "Fix".into(),
            status: "blocked".into(),
            stalled_hours: 7,
            waiting_on: "auth refresh".into(),
            action: "retry".into(),
        }];
        let missions = vec![Mission {
            id: "M-11".into(),
            repo: "C:/repos/alpha".into(),
            status: "done".into(),
            updated_at: 999_000,
            ..Default::default()
        }];
        let mut sent = String::new();
        let rep = build_and_send(
            "2026-11-18",
            1_000_000,
            &lanes,
            &["re-enabled plugin".into()],
            &verdicts,
            &missions,
            &[],
            |m| sent = m.to_string(),
        );
        assert_eq!(rep.open_lanes, 1);
        assert_eq!(rep.closed_lanes, 1);
        assert!(sent.contains("Lanes: 1 open, 1 closed"));
        assert!(sent.contains("Repairs: re-enabled plugin"));
        assert!(sent.contains("M-12 (7h): auth refresh"));
        assert!(sent.contains("C:/repos/alpha"));
    }
}
