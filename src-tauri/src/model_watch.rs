//! New-model badges and deprecation warnings, from Hermes' own model cache.
//!
//! Studio remembers when it first saw each provider/model pair (`studio.db` kv `models.seen`).
//! A pair first seen in the last week is "new"; a pair seen before and missing now is "gone", and is
//! a warning when something still pins it: a queued or running mission, a folder default, or the
//! fallback chain. The first run only records what exists, so nothing is flagged as new on install.

use anyhow::Result;
use serde::Serialize;
use std::collections::BTreeMap;

pub const KV_SEEN: &str = "models.seen";
pub const NEW_FOR_SECS: i64 = 7 * 24 * 3600;

/// provider -> model -> first seen (unix seconds; 0 for the initial snapshot).
pub type Seen = BTreeMap<String, BTreeMap<String, i64>>;

#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct NewModel {
    pub provider: String,
    pub model: String,
    pub first_seen: i64,
}

#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct GoneModel {
    pub provider: String,
    pub model: String,
    /// What still pins it, e.g. `mission M-004`, `folder C:/repo`, `fallback chain`.
    pub used_by: Vec<String>,
}

#[derive(Serialize, Debug, Clone, Default, PartialEq)]
pub struct Changes {
    pub new: Vec<NewModel>,
    pub gone: Vec<GoneModel>,
}

/// Merge the current catalog into `seen` and report changes. Pure.
pub fn diff(
    seen: &mut Seen,
    current: &BTreeMap<String, Vec<String>>,
    now: i64,
    pins: &[(String, String, String)],
) -> Changes {
    let initial = seen.is_empty();
    let mut out = Changes::default();
    for (provider, models) in current {
        let known = seen.entry(provider.clone()).or_default();
        let fresh_provider = known.is_empty() && !initial;
        for m in models {
            let first = *known
                .entry(m.clone())
                .or_insert(if initial { 0 } else { now });
            // A provider seen for the first time (just connected) is not news model by model.
            if first > 0 && now - first < NEW_FOR_SECS && !fresh_provider {
                out.new.push(NewModel {
                    provider: provider.clone(),
                    model: m.clone(),
                    first_seen: first,
                });
            }
        }
        if fresh_provider {
            for v in known.values_mut() {
                *v = 0;
            }
        }
    }
    for (provider, models) in seen.iter() {
        // A provider missing from the cache entirely is a cache gap, not a removal.
        let Some(now_models) = current.get(provider) else {
            continue;
        };
        for m in models.keys() {
            if now_models.contains(m) {
                continue;
            }
            let used_by: Vec<String> = pins
                .iter()
                .filter(|(p, model, _)| p == provider && model == m)
                .map(|(_, _, who)| who.clone())
                .collect();
            out.gone.push(GoneModel {
                provider: provider.clone(),
                model: m.clone(),
                used_by,
            });
        }
    }
    out.new.sort_by_key(|n| std::cmp::Reverse(n.first_seen));
    out.gone.sort_by_key(|g| std::cmp::Reverse(g.used_by.len()));
    out
}

fn pins() -> Vec<(String, String, String)> {
    let mut out = Vec::new();
    if let Ok(list) = crate::missions::list(300) {
        for m in list.iter().filter(|m| {
            matches!(m.status.as_str(), "queued" | "running" | "blocked") && !m.model.is_empty()
        }) {
            out.push((
                m.provider.clone(),
                m.model.clone(),
                format!("mission {}", m.id),
            ));
        }
    }
    for f in crate::model_layer::folder_models() {
        out.push((f.provider, f.model, format!("folder {}", f.folder)));
    }
    for r in crate::model_layer::fallback_chain() {
        out.push((r.provider, r.model, "fallback chain".into()));
    }
    out
}

pub fn check() -> Result<Changes> {
    let current: BTreeMap<String, Vec<String>> = crate::models::catalog()
        .providers
        .into_iter()
        .map(|p| (p.id, p.models))
        .collect();
    let c = crate::store::open()?;
    let mut seen: Seen = crate::store::kv_get(&c, KV_SEEN)?
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    let changes = diff(&mut seen, &current, chrono::Utc::now().timestamp(), &pins());
    crate::store::kv_set(&c, KV_SEEN, &serde_json::to_string(&seen)?)?;
    Ok(changes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cat(v: &[(&str, &[&str])]) -> BTreeMap<String, Vec<String>> {
        v.iter()
            .map(|(p, ms)| (p.to_string(), ms.iter().map(|m| m.to_string()).collect()))
            .collect()
    }

    #[test]
    fn first_run_flags_nothing_then_new_and_gone_are_reported() {
        let mut seen = Seen::new();
        let c0 = diff(&mut seen, &cat(&[("openrouter", &["a", "b"])]), 1000, &[]);
        assert_eq!(
            c0,
            Changes::default(),
            "install does not mark everything new"
        );

        let pins = vec![(
            "openrouter".to_string(),
            "b".to_string(),
            "mission M-1".to_string(),
        )];
        let c1 = diff(&mut seen, &cat(&[("openrouter", &["a", "c"])]), 2000, &pins);
        assert_eq!(c1.new.len(), 1);
        assert_eq!(c1.new[0].model, "c");
        assert_eq!(
            c1.gone,
            vec![GoneModel {
                provider: "openrouter".into(),
                model: "b".into(),
                used_by: vec!["mission M-1".into()]
            }]
        );

        let later = 2000 + NEW_FOR_SECS + 1;
        let c2 = diff(&mut seen, &cat(&[("openrouter", &["a", "c"])]), later, &[]);
        assert!(c2.new.is_empty(), "new for a week only");
    }

    #[test]
    fn a_newly_connected_provider_or_a_missing_one_is_not_news() {
        let mut seen = Seen::new();
        diff(&mut seen, &cat(&[("openrouter", &["a"])]), 1000, &[]);
        let c = diff(
            &mut seen,
            &cat(&[("openrouter", &["a"]), ("nous", &["x", "y"])]),
            2000,
            &[],
        );
        assert!(
            c.new.is_empty(),
            "connecting a provider does not flag its whole list"
        );
        let c = diff(&mut seen, &cat(&[("nous", &["x", "y"])]), 3000, &[]);
        assert!(
            c.gone.is_empty(),
            "a provider absent from the cache is a gap, not removals"
        );
    }
}
