//! Single-instance guard without a native dependency: the first instance listens on a
//! loopback port, and any later launch asks it to come to the front and then exits.
//!
//! This is the difference between "the app is open behind six windows" and "the app did not
//! start". A second GUI instance would also start a second heartbeat, a second watcher and a
//! second set of task threads, which is why this is a correctness fix and not a nicety.

use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::time::Duration;

use tauri::{AppHandle, Manager};

const PORT: u16 = 47_823;
const ACTIVATE: &str = "agentos-studio:activate";

fn addr() -> SocketAddr {
    SocketAddr::from((Ipv4Addr::LOCALHOST, PORT))
}

/// True when another instance is already listening. Asks it to focus, then the caller exits.
pub fn already_running() -> bool {
    let Ok(mut s) = TcpStream::connect_timeout(&addr(), Duration::from_millis(400)) else {
        return false;
    };
    let _ = s.set_write_timeout(Some(Duration::from_millis(400)));
    let _ = s.write_all(ACTIVATE.as_bytes());
    true
}

/// Claim the port and serve activation requests on a background thread.
pub fn claim(app: AppHandle) -> anyhow::Result<()> {
    let listener = TcpListener::bind(addr())?;
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut buf = [0u8; 64];
            let mut s = stream;
            let _ = s.set_read_timeout(Some(Duration::from_millis(500)));
            let n = s.read(&mut buf).unwrap_or(0);
            if String::from_utf8_lossy(&buf[..n]).starts_with("agentos-studio") {
                focus(&app);
            }
        }
    });
    Ok(())
}

/// Bring the existing window to the front, restoring it if minimised or hidden.
pub fn focus(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_is_stable_and_local_only() {
        assert_eq!(addr().ip().to_string(), "127.0.0.1");
        assert_eq!(addr().port(), PORT);
    }

    #[test]
    fn probe_returns_false_when_nothing_listens() {
        // Port 47823 is normally free in the test environment; if a real Studio is running
        // this asserts the opposite, so only run the negative case when the bind succeeds.
        match TcpListener::bind(addr()) {
            Ok(l) => {
                drop(l);
                assert!(
                    !already_running(),
                    "nothing should be listening after the probe bind"
                );
            }
            Err(_) => {
                assert!(
                    already_running(),
                    "a listener is present, so the probe must see it"
                );
            }
        }
    }
}
