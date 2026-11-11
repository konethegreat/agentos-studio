//! "Set up everything": the plan behind the one-click setup (docs/ONE-CLICK-SETUP.md).
//!
//! Everything here is pure so the order, the elevated batch and the generated script are unit
//! tested; `bootstrapper` runs the plan and streams its progress.

use serde::Serialize;

use crate::bootstrapper::Component;

/// A system tool installed by the operating system's package manager.
pub struct SystemPkg {
    pub component: &'static str,
    pub winget: &'static str,
    pub brew: &'static str,
    /// Linux package names for apt and dnf; empty when the distribution does not ship it.
    pub apt: &'static str,
    pub dnf: &'static str,
}

pub const SYSTEM_PKGS: [SystemPkg; 4] = [
    SystemPkg {
        component: "git",
        winget: "Git.Git",
        brew: "git",
        apt: "git",
        dnf: "git",
    },
    SystemPkg {
        component: "node",
        winget: "OpenJS.NodeJS.LTS",
        brew: "node",
        apt: "nodejs npm",
        dnf: "nodejs npm",
    },
    SystemPkg {
        component: "gh",
        winget: "GitHub.cli",
        brew: "gh",
        apt: "gh",
        dnf: "gh",
    },
    // Claude Code: winget on Windows; elsewhere it is installed with npm as a user step.
    SystemPkg {
        component: "claude-cli",
        winget: "Anthropic.ClaudeCode",
        brew: "",
        apt: "",
        dnf: "",
    },
];

/// User-level steps, in the order they depend on each other (Hermes before its plugin).
pub const USER_ORDER: [&str; 9] = [
    "path",
    "claude-cli",
    "hermes",
    "hermes-plugin",
    "vault",
    "bus-selftest",
    "mcp-hermes",
    "mcp-claude",
    "mcp-qoder",
];

/// Steps that need a person (a browser sign-in or a choice), always last and in this order.
pub const SIGN_INS: [&str; 3] = ["gh", "claude-auth", "hermes-model"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Os {
    Windows,
    MacOs,
    Linux,
}

impl Os {
    pub fn current() -> Os {
        if cfg!(windows) {
            Os::Windows
        } else if cfg!(target_os = "macos") {
            Os::MacOs
        } else {
            Os::Linux
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct Plan {
    /// Components installed by the package manager in one elevated pass.
    pub system: Vec<String>,
    /// winget is missing and is installed first, inside the same elevated pass.
    pub install_winget: bool,
    /// The user answers exactly one admin prompt (UAC on Windows, pkexec on Linux).
    pub admin_prompt: bool,
    /// Steps that run as the user, in order.
    pub user: Vec<String>,
    /// Sign-ins only a person can finish, in order.
    pub sign_ins: Vec<String>,
}

impl Plan {
    pub fn is_empty(&self) -> bool {
        self.system.is_empty() && self.user.is_empty() && self.sign_ins.is_empty()
    }
}

/// Build the plan from the Setup checks. Optional items are never part of it. Pure.
pub fn plan(components: &[Component], os: Os, winget_present: bool) -> Plan {
    let broken = |id: &str| {
        components
            .iter()
            .any(|c| c.id == id && !c.ok && !c.optional)
    };
    // A tool whose check failed because it is missing (not because it needs a sign-in).
    let missing = |id: &str| {
        components
            .iter()
            .any(|c| c.id == id && !c.ok && !c.optional && c.action.starts_with("Install"))
    };
    let mut p = Plan::default();
    for pkg in SYSTEM_PKGS.iter() {
        if !missing(pkg.component) {
            continue;
        }
        let by_os = match os {
            Os::Windows => !pkg.winget.is_empty(),
            Os::MacOs => !pkg.brew.is_empty(),
            Os::Linux => !pkg.apt.is_empty(),
        };
        if by_os {
            p.system.push(pkg.component.to_string());
        }
    }
    p.install_winget = os == Os::Windows && !p.system.is_empty() && !winget_present;
    p.admin_prompt = !p.system.is_empty() && os != Os::MacOs;
    for id in USER_ORDER.iter().chain(["mcp-antigravity"].iter()) {
        if broken(id) && !p.system.iter().any(|s| s == id) {
            p.user.push(id.to_string());
        }
    }
    for id in SIGN_INS {
        // gh is a sign-in when it is installed but signed out, or right after its install.
        let wanted = if id == "gh" { broken("gh") } else { broken(id) };
        if wanted {
            p.sign_ins.push(id.to_string());
        }
    }
    p
}

/// Single-quote a value for PowerShell. Pure.
fn ps_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// The elevated PowerShell script for the Windows system pass. It appends progress lines to
/// `log` and writes `{"<id>": <exit code>}` to `result`, so Studio can stream and judge it from
/// the unelevated side. Pure.
pub fn windows_script(
    winget_ids: &[&str],
    install_winget: bool,
    log: &str,
    result: &str,
) -> String {
    let mut s = String::new();
    s.push_str("$ErrorActionPreference = 'Continue'\r\n");
    s.push_str(&format!(
        "$log = {}\r\n$result = {}\r\n",
        ps_quote(log),
        ps_quote(result)
    ));
    s.push_str("function Say($t) { Add-Content -LiteralPath $log -Value $t -Encoding UTF8 }\r\n");
    s.push_str("$codes = @{}\r\n");
    if install_winget {
        s.push_str(
            "Say 'Installing winget (App Installer)'\r\n\
             try { Add-AppxPackage -RegisterByFamilyName -MainPackage Microsoft.DesktopAppInstaller_8wekyb3d8bbwe -ErrorAction Stop } catch {\r\n\
             \x20 $b = Join-Path $env:TEMP 'AppInstaller.msixbundle'\r\n\
             \x20 Invoke-WebRequest -UseBasicParsing -Uri 'https://aka.ms/getwinget' -OutFile $b\r\n\
             \x20 Add-AppxPackage -Path $b\r\n\
             }\r\n\
             $env:Path = [Environment]::GetEnvironmentVariable('Path','Machine') + ';' + [Environment]::GetEnvironmentVariable('Path','User') + ';' + (Join-Path $env:LOCALAPPDATA 'Microsoft\\WindowsApps')\r\n",
        );
    }
    for id in winget_ids {
        let q = ps_quote(id);
        s.push_str(&format!(
            "Say ('Installing ' + {q})\r\n\
             winget install --id {q} -e --silent --accept-package-agreements --accept-source-agreements --disable-interactivity 2>&1 | ForEach-Object {{ Say $_ }}\r\n\
             $codes[{q}] = $LASTEXITCODE\r\n"
        ));
    }
    s.push_str(
        "$codes | ConvertTo-Json -Compress | Set-Content -LiteralPath $result -Encoding UTF8\r\n",
    );
    s.push_str("Say 'Elevated pass finished'\r\n");
    s
}

/// winget exit codes that mean the package is in place: success, already installed, and no
/// applicable upgrade for an installed package. Pure.
pub fn winget_ok(code: i64) -> bool {
    matches!(code, 0 | -1978335189 | -1978335135)
}

/// The Linux package names for the system pass, for apt or dnf. Pure.
pub fn linux_packages(components: &[String], dnf: bool) -> Vec<String> {
    SYSTEM_PKGS
        .iter()
        .filter(|p| components.iter().any(|c| c == p.component))
        .flat_map(|p| (if dnf { p.dnf } else { p.apt }).split_whitespace())
        .map(String::from)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(id: &str, ok: bool, action: &str) -> Component {
        Component {
            id: id.into(),
            label: id.into(),
            pillar: 0,
            ok,
            detail: String::new(),
            action: action.into(),
            optional: false,
        }
    }

    fn fresh_machine() -> Vec<Component> {
        vec![
            c("git", false, "Install"),
            c("gh", false, "Install"),
            c("node", false, "Install"),
            c("path", false, "Fix PATH"),
            c("claude-cli", false, "Install"),
            c("claude-auth", false, "Install Claude Code first"),
            c("hermes-plugin", false, "Install Hermes first"),
            c("hermes", false, "Install"),
            c("hermes-model", false, "Configure"),
            c("vault", false, "Initialize"),
            c("bus-selftest", false, "Repair"),
            c("mcp-hermes", false, "Wire Config"),
            Component {
                optional: true,
                ..c("notion", false, "Connect")
            },
        ]
    }

    #[test]
    fn a_fresh_windows_machine_gets_one_admin_prompt_then_user_steps_then_sign_ins() {
        let p = plan(&fresh_machine(), Os::Windows, false);
        assert_eq!(p.system, ["git", "node", "gh", "claude-cli"]);
        assert!(p.install_winget && p.admin_prompt);
        // Hermes before its plugin; Claude Code is not reinstalled as a user step.
        assert_eq!(
            p.user,
            [
                "path",
                "hermes",
                "hermes-plugin",
                "vault",
                "bus-selftest",
                "mcp-hermes"
            ]
        );
        assert_eq!(p.sign_ins, ["gh", "claude-auth", "hermes-model"]);
        assert!(!p.user.iter().chain(&p.sign_ins).any(|s| s == "notion"));
    }

    #[test]
    fn elsewhere_claude_code_is_a_user_step_and_mac_needs_no_admin_prompt() {
        let mac = plan(&fresh_machine(), Os::MacOs, false);
        assert_eq!(mac.system, ["git", "node", "gh"]);
        assert!(!mac.admin_prompt && !mac.install_winget);
        assert_eq!(
            mac.user[..2],
            ["path".to_string(), "claude-cli".to_string()]
        );
        let linux = plan(&fresh_machine(), Os::Linux, true);
        assert!(linux.admin_prompt);
        assert_eq!(
            linux_packages(&linux.system, false),
            ["git", "nodejs", "npm", "gh"]
        );
    }

    #[test]
    fn a_signed_out_tool_is_only_a_sign_in_and_a_healthy_machine_has_no_plan() {
        let mut m: Vec<Component> = fresh_machine()
            .into_iter()
            .map(|x| Component { ok: true, ..x })
            .collect();
        m[1] = c("gh", false, "Sign in");
        let p = plan(&m, Os::Windows, true);
        assert!(p.system.is_empty() && !p.admin_prompt && !p.install_winget);
        assert_eq!(p.sign_ins, ["gh"]);
        let healthy: Vec<Component> = fresh_machine()
            .into_iter()
            .map(|x| Component { ok: true, ..x })
            .collect();
        assert!(plan(&healthy, Os::Windows, false).is_empty());
    }

    /// Live: run the generated script unelevated for a package that is already installed and
    /// check the log and result contract the runner reads.
    /// `cargo test --lib -- --ignored windows_script_live --nocapture`
    #[test]
    #[ignore]
    #[cfg(windows)]
    fn windows_script_live() {
        let dir = std::env::temp_dir().join(format!("setup-flow-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (log, res, ps1) = (dir.join("l.txt"), dir.join("r.json"), dir.join("s.ps1"));
        std::fs::write(
            &ps1,
            windows_script(
                &["Git.Git"],
                false,
                &log.display().to_string(),
                &res.display().to_string(),
            ),
        )
        .unwrap();
        let st = std::process::Command::new("powershell")
            .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
            .arg(&ps1)
            .status()
            .unwrap();
        assert!(st.success());
        let text = std::fs::read_to_string(&log).unwrap();
        println!("{text}");
        assert!(text.contains("Installing 'Git.Git'") || text.contains("Installing Git.Git"));
        assert!(text.contains("Elevated pass finished"));
        let r = std::fs::read_to_string(&res).unwrap();
        let v: serde_json::Value = serde_json::from_str(r.trim_start_matches('\u{feff}')).unwrap();
        let code = v
            .get("Git.Git")
            .and_then(serde_json::Value::as_i64)
            .unwrap();
        println!("exit code {code}");
        assert!(
            winget_ok(code),
            "an installed package reads as in place: {code}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_elevated_script_quotes_values_and_reports_every_package() {
        let s = windows_script(
            &["Git.Git", "Odd'Id"],
            true,
            "C:\\a b\\log.txt",
            "C:\\r.json",
        );
        assert!(s.contains("$log = 'C:\\a b\\log.txt'"));
        assert!(s.contains("'Odd''Id'"), "single quotes are doubled");
        assert_eq!(s.matches("winget install --id").count(), 2);
        assert!(s.contains("DesktopAppInstaller_8wekyb3d8bbwe"));
        assert!(s.contains("ConvertTo-Json"));
        assert!(!windows_script(&["Git.Git"], false, "l", "r").contains("AppInstaller"));
        assert!(winget_ok(0) && winget_ok(-1978335189) && !winget_ok(1));
    }
}
