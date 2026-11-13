//! Offline awareness.
//!
//! When the network is gone, a cloud model fails at the worst possible moment. This module answers
//! two questions cheaply: is the network reachable, and what can run locally. It never changes the
//! active provider on its own; pinning a local model stays a deliberate choice, offered by the
//! existing provider switcher once we can tell the user there is something to pin.

use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use serde::Serialize;

#[derive(Debug, Clone, Serialize, Default)]
pub struct OfflineStatus {
    pub online: bool,
    pub local_models: Vec<String>,
    pub note: String,
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
    let note = match (online, models.is_empty()) {
        (true, true) => "Online. No local runtime is running.".into(),
        (true, false) => format!("Online. {} local model(s) available.", models.len()),
        (false, false) => format!(
            "Offline. Missions wait and resume when the network returns; {} local model(s) can still run if you pin one in Providers.",
            models.len()
        ),
        (false, true) => {
            "Offline. Missions wait and resume when the network returns; nothing is marked stuck meanwhile.".into()
        }
    };
    OfflineStatus {
        online,
        local_models: models,
        note,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
