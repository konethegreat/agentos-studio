//! The router, in shadow mode.
//!
//! It scores models from what actually happened in past missions: how often the reviewer approved,
//! and what the work cost. It never switches anything. It prints what it would choose, so you can
//! disagree with it before it is allowed to act.
//!
//! With few samples the score is smoothed toward the middle, because one approval out of one is not
//! evidence. The smoothing falls away as samples accumulate.

use anyhow::Result;
use serde::Serialize;

use crate::missions;

#[derive(Debug, Clone, Serialize)]
pub struct Suggestion {
    pub role: String,
    pub provider: String,
    pub model: String,
    pub attempts: usize,
    pub approvals: usize,
    pub score: f64,
    pub avg_cost_usd: f64,
    pub reason: String,
}

/// Prior strength: how many imaginary neutral outcomes every candidate starts with.
const PRIOR: f64 = 2.0;

/// Laplace-smoothed approval rate, minus a small cost penalty. Pure, so it is testable.
pub fn score(approvals: usize, attempts: usize, avg_cost: f64) -> f64 {
    let rate = (approvals as f64 + PRIOR * 0.5) / (attempts as f64 + PRIOR);
    let cost_penalty = (avg_cost / 5.0).min(0.3);
    (rate * 100.0) - cost_penalty
}

pub fn suggestions() -> Result<Vec<Suggestion>> {
    let all = missions::list(500)?;
    let mut buckets: std::collections::BTreeMap<(String, String, String), (usize, usize, f64)> =
        std::collections::BTreeMap::new();
    for m in all
        .iter()
        .filter(|m| m.status == "done" || m.review_verdict.is_some())
    {
        let key = (m.role.clone(), m.provider.clone(), m.model.clone());
        let e = buckets.entry(key).or_insert((0, 0, 0.0));
        e.0 += 1;
        if m.review_verdict.as_deref() == Some("approve") {
            e.1 += 1;
        }
        e.2 += m.spent_usd;
    }
    let mut out: Vec<Suggestion> = buckets
        .into_iter()
        .map(|((role, provider, model), (attempts, approvals, spent))| {
            let avg = if attempts > 0 {
                spent / attempts as f64
            } else {
                0.0
            };
            let s = score(approvals, attempts, avg);
            let reason = if attempts < 3 {
                format!("{approvals}/{attempts} approved, too few samples to trust yet")
            } else {
                format!("{approvals}/{attempts} approved, ${avg:.2} average")
            };
            Suggestion {
                role,
                provider,
                model: if model.is_empty() {
                    "inherit".into()
                } else {
                    model
                },
                attempts,
                approvals,
                score: (s * 10.0).round() / 10.0,
                avg_cost_usd: avg,
                reason,
            }
        })
        .collect();
    out.sort_by(|a, b| {
        a.role.cmp(&b.role).then_with(|| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
    });
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evidence_beats_a_single_lucky_run() {
        let lucky = score(1, 1, 0.0);
        let proven = score(8, 10, 0.0);
        assert!(proven > lucky, "{proven} should beat {lucky}");
    }

    #[test]
    fn cost_penalises_without_dominating() {
        let cheap = score(3, 4, 0.0);
        let dear = score(3, 4, 50.0);
        assert!(dear < cheap);
        assert!(dear > 50.0, "cost must not erase a good approval rate");
    }

    #[test]
    fn empty_history_is_neutral_not_confident() {
        let s = score(0, 0, 0.0);
        assert!((s - 50.0).abs() < 1.0, "got {s}");
    }
}
