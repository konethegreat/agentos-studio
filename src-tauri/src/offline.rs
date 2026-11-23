//! Offline awareness and automatic local pinning.
//!
//! When the network is gone, a cloud model fails at the worst possible moment. This module
//! answers two questions cheaply: is the network reachable, and what can run locally. While the
//! network is down it pins one local Ollama model in Hermes; when the network returns it puts the
//! previous provider back. The decision is a pure function; applying it goes through the existing
//! Hermes switch, which verifies the read-back, and the previous provider is kept in the store so
//! a restart mid-episode still restores. If the owner switches providers during the episode, the
//! record is dropped without touching config.

use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::store;

/// Emitted every cycle with the current status.
pub const EV_OFFLINE: &str = "agentos://offline";
const KEY_AUTO: &str = "offline.auto_local";
const KEY_PIN: &str = "offline.pin";

#[derive(Debug, Clone, Serialize, Default)]
pub struct OfflineStatus {
    pub online: bool,
    pub local_models: Vec<String>,
    pub note: String,
    /// Automatic pinning is on unless the owner turned it off in the banner.
    pub auto_local: bool,
    /// Present while a local model is pinned for this offline episode.
    pub pin: Option<Pin>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Pin {
    /// Provider to restore when the network returns.
    pub provider: String,
    pub model: String,
    pub base_url: Option<String>,
    /// The local model Studio switched to.
    pub local_model: String,
    pub since: i64,
}

/// What the current state calls for.
#[derive(Debug, Clone, PartialEq)]
pub enum Decision {
    None,
    PinLocal { model: String },
    Restore { pin: Pin },
    Forget { pin: Pin },
}

/// The active Hermes provider, as far as the pin logic cares.
#[derive(Debug, Clone, PartialEq)]
pub struct Active {
    pub provider: String,
    pub model: String,
    /// The catalog can switch back to this provider after the episode.
    pub restorable: bool,
}

/// The whole pin rule, with no disk or network access, so the table is testable.
pub fn decide(
    online: bool,
    auto: bool,
    pin: Option<&Pin>,
    active: Option<&Active>,
    models: &[String],
) -> Decision {
    if let Some(p) = pin {
        // Without a readable config there is nothing to verify; leave the record for next cycle.
        let Some(a) = active else {
            return Decision::None;
        };
        let still_pinned = a.provider == "custom" && a.model == p.local_model;
        if !still_pinned {
            return Decision::Forget { pin: p.clone() };
        }
        // Reconnected, or the owner turned the automation off: put the old provider back.
        if online || !auto {
            return Decision::Restore { pin: p.clone() };
        }
        return Decision::None;
    }
    if online || !auto {
        return Decision::None;
    }
    let Some(a) = active else {
        return Decision::None;
    };
    // Already local, not restorable through the catalog, or nothing pulled: nothing to pin.
    if !a.restorable || a.provider == "custom" || models.is_empty() {
        return Decision::None;
    }
    Decision::PinLocal {
        model: models[0].clone(),
    }
}

/// Whether new work may start and silence may be judged stuck: the network is up, or a local
/// model is pinned. One rule, so the scheduler and the stuck watchdog cannot disagree.
pub fn can_run(online: bool, pinned: bool) -> bool {
    online || pinned
}

pub fn network_usable() -> bool {
    can_run(crate::agent_limits::online(), pinned_locally())
}

pub fn auto_local() -> bool {
    store::open()
        .ok()
        .and_then(|c| store::kv_get(&c, KEY_AUTO).ok().flatten())
        .map(|v| v != "off")
        .unwrap_or(true)
}

pub fn set_auto_local(on: bool) -> Result<()> {
    let c = store::open()?;
    store::kv_set(&c, KEY_AUTO, if on { "on" } else { "off" })
}

fn save_pin(p: &Pin) -> Result<()> {
    let c = store::open()?;
    store::kv_set(&c, KEY_PIN, &serde_json::to_string(p)?)
}

fn clear_pin() -> Result<()> {
    let c = store::open()?;
    store::kv_set(&c, KEY_PIN, "")
}

pub fn pin_record() -> Option<Pin> {
    let c = store::open().ok()?;
    let raw = store::kv_get(&c, KEY_PIN).ok()??;
    serde_json::from_str(&raw).ok()
}

pub fn pinned_locally() -> bool {
    pin_record().is_some()
}

/// Apply a decision through the existing provider switch, which verifies that Hermes read the
/// values back. A failed switch leaves the record as it was, so the next cycle tries again.
pub fn apply(d: &Decision) -> Result<()> {
    match d {
        Decision::None => Ok(()),
        Decision::PinLocal { model } => {
            let cfg = crate::hermes_engine::read_config()?;
            let prev = crate::hermes_engine::model_config(&cfg);
            let pin = Pin {
                provider: prev.provider,
                model: prev.model,
                base_url: prev.base_url,
                local_model: model.clone(),
                since: store::now(),
            };
            crate::hermes_engine::set_active_provider("local", model, None, None)?;
            save_pin(&pin)
        }
        Decision::Restore { pin } => {
            crate::hermes_engine::set_active_provider(
                &pin.provider,
                &pin.model,
                None,
                pin.base_url.as_deref(),
            )?;
            clear_pin()
        }
        Decision::Forget { .. } => clear_pin(),
    }
}

/// One pass: probe, decide, apply, report. Called by the background loop every minute.
pub fn cycle() -> OfflineStatus {
    let mut st = status();
    let active = active_provider();
    let d = decide(
        st.online,
        st.auto_local,
        st.pin.as_ref(),
        active.as_ref(),
        &st.local_models,
    );
    if let Err(e) = apply(&d) {
        eprintln!("offline: {e:#}");
    }
    if !matches!(d, Decision::None) {
        st.pin = pin_record();
        st.note = note_for(st.online, &st.local_models, st.auto_local, st.pin.as_ref());
    }
    st
}

fn active_provider() -> Option<Active> {
    let cfg = crate::hermes_engine::read_config().ok()?;
    let m = crate::hermes_engine::model_config(&cfg);
    let restorable = crate::hermes_engine::provider_def(&m.provider).is_some();
    Some(Active {
        provider: m.provider,
        model: m.model,
        restorable,
    })
}

/// A single short TCP probe. Cheap, no DNS dependency beyond the host, no retries.
pub fn reachable(host: &str, port: u16, timeout_ms: u64) -> bool {
    let target = format!("{host}:{port}");
    let Ok(addr) = target
        .to_socket_addrs()
        .ok()
        .and_then(|mut a| a.next())
        .ok_or(())
    else {
        return false;
    };
    TcpStream::connect_timeout(&addr, Duration::from_millis(timeout_ms)).is_ok()
}

/// Models the local runtime reports, if it is running.
pub fn local_models() -> Vec<String> {
    let req = ureq::http::Request::builder()
        .uri("http://localhost:11434/api/tags")
        .body(String::new());
    let Ok(req) = req else { return Vec::new() };
    let Ok(res) = ureq::run(req) else {
        return Vec::new();
    };
    let Ok(text) = res.into_body().read_to_string() else {
        return Vec::new();
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
        return Vec::new();
    };
    v.get("models")
        .and_then(|m| m.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|m| m.get("name").and_then(|n| n.as_str()).map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

pub fn status() -> OfflineStatus {
    let online = reachable("1.1.1.1", 443, 800) || reachable("api.openrouter.ai", 443, 800);
    let models = local_models();
    let auto = auto_local();
    let pin = pin_record();
    OfflineStatus {
        online,
        note: note_for(online, &models, auto, pin.as_ref()),
        local_models: models,
        auto_local: auto,
        pin,
    }
}

fn note_for(online: bool, models: &[String], auto: bool, pin: Option<&Pin>) -> String {
    if online {
        return match models.is_empty() {
            true => "Online. No local runtime is running.".into(),
            false => format!("Online. {} local model(s) available.", models.len()),
        };
    }
    if let Some(p) = pin {
        return format!(
            "Offline. Running on {} locally; the previous provider returns when the network does.",
            p.local_model
        );
    }
    if models.is_empty() {
        return if auto {
            "Offline. Missions wait and resume when the network returns; nothing is marked stuck meanwhile.".into()
        } else {
            "Offline. Automatic local pinning is off; missions wait and resume when the network returns.".into()
        };
    }
    if auto {
        format!(
            "Offline. {} local model(s) available; pinning one automatically.",
            models.len()
        )
    } else {
        format!(
            "Offline. {} local model(s) available; automatic pinning is off, so missions wait.",
            models.len()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn active(provider: &str, model: &str) -> Active {
        Active {
            provider: provider.into(),
            model: model.into(),
            restorable: true,
        }
    }

    fn pin() -> Pin {
        Pin {
            provider: "anthropic".into(),
            model: "claude-opus-4".into(),
            base_url: None,
            local_model: "qwen3:14b".into(),
            since: 1_700_000_000,
        }
    }

    #[test]
    fn the_decision_table_is_what_the_banner_promises() {
        let models = vec!["qwen3:14b".to_string(), "llama3.2:3b".to_string()];
        let cloud = active("anthropic", "claude-opus-4");
        let local = active("custom", "qwen3:14b");
        let p = pin();

        // Online with no pin: nothing to do.
        assert_eq!(
            decide(true, true, None, Some(&cloud), &models),
            Decision::None
        );
        // Offline with automatic pinning off: nothing starts, config stays.
        assert_eq!(
            decide(false, false, None, Some(&cloud), &models),
            Decision::None
        );
        // Offline with nothing pulled: nothing to pin.
        assert_eq!(decide(false, true, None, Some(&cloud), &[]), Decision::None);
        // Already on a local model: do not pin over the owner's choice.
        assert_eq!(
            decide(false, true, None, Some(&local), &models),
            Decision::None
        );
        // A provider the catalog cannot switch back to: do not pin what cannot be restored.
        let mut auto_provider = active("auto", "some-model");
        auto_provider.restorable = false;
        assert_eq!(
            decide(false, true, None, Some(&auto_provider), &models),
            Decision::None
        );
        // Unreadable config: never act blind.
        assert_eq!(decide(false, true, None, None, &models), Decision::None);
        // The normal case: offline, cloud active, a model pulled.
        assert_eq!(
            decide(false, true, None, Some(&cloud), &models),
            Decision::PinLocal {
                model: "qwen3:14b".into()
            }
        );

        // Still pinned while offline: hold.
        assert_eq!(
            decide(false, true, Some(&p), Some(&local), &models),
            Decision::None
        );
        // Reconnected: put the old provider back.
        assert_eq!(
            decide(true, true, Some(&p), Some(&local), &models),
            Decision::Restore { pin: p.clone() }
        );
        // The owner turned automatic pinning off mid-episode: also restore.
        assert_eq!(
            decide(false, false, Some(&p), Some(&local), &models),
            Decision::Restore { pin: p.clone() }
        );
        // The owner switched providers during the episode: drop the record, touch nothing.
        assert_eq!(
            decide(true, true, Some(&p), Some(&cloud), &models),
            Decision::Forget { pin: p.clone() }
        );
        assert_eq!(
            decide(false, true, Some(&p), Some(&cloud), &models),
            Decision::Forget { pin: p.clone() }
        );
    }

    #[test]
    fn work_may_run_on_the_network_or_on_a_local_pin() {
        assert!(can_run(true, false));
        assert!(can_run(false, true));
        assert!(!can_run(false, false));
    }

    #[test]
    fn the_pin_record_round_trips_and_can_be_cleared() {
        let d = tempfile::tempdir().unwrap();
        let _g = store::test_env_lock();
        store::set_test_db(Some(d.path().join("studio.db")));
        assert!(pin_record().is_none());
        assert!(!pinned_locally());
        let p = pin();
        save_pin(&p).unwrap();
        assert_eq!(pin_record(), Some(p.clone()));
        assert!(pinned_locally());
        clear_pin().unwrap();
        assert!(pin_record().is_none());
        assert!(!pinned_locally());
        store::set_test_db(None);
    }

    #[test]
    fn automatic_pinning_is_on_until_the_owner_turns_it_off() {
        let d = tempfile::tempdir().unwrap();
        let _g = store::test_env_lock();
        store::set_test_db(Some(d.path().join("studio.db")));
        assert!(auto_local());
        set_auto_local(false).unwrap();
        assert!(!auto_local());
        set_auto_local(true).unwrap();
        assert!(auto_local());
        store::set_test_db(None);
    }

    #[test]
    fn probe_handles_unresolvable_and_closed_targets() {
        assert!(!reachable("does-not-exist.invalid", 9, 200));
        assert!(!reachable("127.0.0.1", 9, 200));
    }

    #[test]
    fn status_always_explains_itself() {
        let s = status();
        assert!(!s.note.is_empty());
    }
}
