//! Start at login (Beyond B2.6): register AgentOS Studio in the user's Windows Run key so
//! the night shift starts after a reboot without the owner.

use std::path::Path;

use anyhow::Result;

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const VALUE_NAME: &str = "AgentOS Studio";

/// The command written to the Run key: launch minimized to the tray so a reboot does not
/// pop a window over the owner's screen. Pure.
pub fn command_for_exe(exe: &Path) -> String {
    format!("\"{}\" --minimized", exe.display())
}

/// Did the user start the app with `--minimized`? Pure.
pub fn start_minimized<I, S>(args: I) -> bool
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    args.into_iter().any(|a| a.as_ref() == "--minimized")
}

/// Is AgentOS Studio currently set to start at login?
pub fn is_enabled() -> bool {
    #[cfg(windows)]
    {
        use winreg::enums::HKEY_CURRENT_USER;
        use winreg::RegKey;
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let Ok(key) = hkcu.open_subkey(RUN_KEY) else {
            return false;
        };
        key.get_value::<String, _>(VALUE_NAME)
            .map(|v| !v.trim().is_empty())
            .unwrap_or(false)
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// Enable or disable starting AgentOS Studio at login.
pub fn set_enabled(enabled: bool) -> Result<bool> {
    #[cfg(windows)]
    {
        use winreg::enums::{HKEY_CURRENT_USER, KEY_SET_VALUE};
        use winreg::RegKey;
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        if enabled {
            let exe = std::env::current_exe()?;
            let (key, _) = hkcu.create_subkey(RUN_KEY)?;
            key.set_value(VALUE_NAME, &command_for_exe(&exe))?;
            Ok(true)
        } else {
            if let Ok(key) = hkcu.open_subkey_with_flags(RUN_KEY, KEY_SET_VALUE) {
                let _ = key.delete_value(VALUE_NAME);
            }
            Ok(false)
        }
    }
    #[cfg(not(windows))]
    {
        let _ = enabled;
        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_quotes_the_executable_and_starts_minimized() {
        let p =
            Path::new(r"C:\Users\Konet\AppData\Local\Programs\AgentOS Studio\agentos-studio.exe");
        let cmd = command_for_exe(p);
        assert!(
            cmd.starts_with('"') && cmd.ends_with(" --minimized"),
            "{cmd}"
        );
        assert!(cmd.contains("agentos-studio.exe"), "{cmd}");
    }

    #[test]
    fn start_minimized_detects_the_flag() {
        assert!(start_minimized(["agentos-studio.exe", "--minimized"]));
        assert!(!start_minimized(["agentos-studio.exe"]));
    }
}
