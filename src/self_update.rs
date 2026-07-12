//! Check GitHub releases for a newer raven-tui and optionally reinstall.
//!
//! - `raven-tui --update` — always check and prompt (or install with `--yes`)
//! - Interactive TUI startup — at most once per day (unless disabled)

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::io::{self, IsTerminal, Write};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

const RELEASE_API: &str = "https://api.github.com/repos/ixmon/raven-harness/releases/latest";
const INSTALL_SCRIPT_URL: &str =
    "https://raw.githubusercontent.com/ixmon/raven-harness/main/scripts/install.sh";
/// Minimum seconds between automatic (non-`--update`) checks.
const DAILY_SECS: u64 = 24 * 60 * 60;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct UpdateCheckState {
    /// Unix timestamp of last automatic check.
    last_check_unix: u64,
    /// Version the user last declined to install (skip until a newer tag appears).
    #[serde(default)]
    skipped_version: Option<String>,
}

fn raven_home() -> PathBuf {
    std::env::var_os("RAVEN_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            dirs_home()
                .map(|h| h.join(".raven-hotel"))
                .unwrap_or_else(|| PathBuf::from(".raven-hotel"))
        })
}

fn dirs_home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

fn state_path() -> PathBuf {
    raven_home().join("update_check.json")
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn load_state() -> UpdateCheckState {
    let path = state_path();
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_state(state: &UpdateCheckState) -> Result<()> {
    let path = state_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let data = serde_json::to_string_pretty(state)?;
    std::fs::write(&path, data).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

/// Running binary version from Cargo (no `v` prefix).
pub fn current_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Normalize `v0.1.9` / `0.1.9` → comparable string without leading `v`.
pub fn normalize_tag(tag: &str) -> &str {
    tag.trim().trim_start_matches('v').trim_start_matches('V')
}

/// True if `latest` is strictly newer than `current` (simple semver x.y.z).
pub fn is_newer(latest: &str, current: &str) -> bool {
    let l = parse_semver(normalize_tag(latest));
    let c = parse_semver(normalize_tag(current));
    l > c
}

fn parse_semver(s: &str) -> (u64, u64, u64) {
    let mut parts = s.split(['.', '-', '+']);
    let major = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    let minor = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    let patch = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    (major, minor, patch)
}

/// Fetch latest release tag from GitHub (e.g. `v0.1.9`).
pub fn fetch_latest_tag() -> Result<String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(12))
        .user_agent(format!("raven-tui/{}", current_version()))
        .build()
        .context("HTTP client")?;

    let resp = client
        .get(RELEASE_API)
        .header("Accept", "application/vnd.github+json")
        .send()
        .context("requesting GitHub releases/latest")?;

    if !resp.status().is_success() {
        bail!("GitHub releases API returned HTTP {}", resp.status());
    }

    let body: serde_json::Value = resp.json().context("parsing release JSON")?;
    let tag = body
        .get("tag_name")
        .and_then(|t| t.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow::anyhow!("release JSON missing tag_name"))?;
    Ok(tag.to_string())
}

fn prompt_yes_no(question: &str) -> Result<bool> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Ok(false);
    }
    print!("{question}");
    let _ = io::stdout().flush();
    let mut line = String::new();
    io::stdin().read_line(&mut line)?;
    let a = line.trim();
    Ok(matches!(a, "y" | "Y" | "yes" | "Yes" | "YES"))
}

/// Re-run the official install script (same as curl | bash install).
pub fn run_install_script() -> Result<()> {
    eprintln!("==> Downloading and running install script…");
    let status = std::process::Command::new("bash")
        .arg("-c")
        .arg(format!(
            "curl -fsSL {INSTALL_SCRIPT_URL} | RAVEN_INSTALL_NO_PATH=1 bash"
        ))
        .status()
        .context("spawning install script")?;
    if !status.success() {
        bail!("install script exited with {status}");
    }
    Ok(())
}

/// Force update check (`--update`). With `assume_yes`, skip the y/n prompt.
/// Returns whether an install was performed.
pub fn run_forced_update(assume_yes: bool) -> Result<bool> {
    let current = current_version();
    eprintln!("==> Checking for updates (running {current})…");
    let latest = fetch_latest_tag().context("could not fetch latest release")?;
    eprintln!("==> Latest release: {latest}");

    if !is_newer(&latest, current) {
        eprintln!("==> Already up to date ({current}).");
        return Ok(false);
    }

    let do_it = if assume_yes {
        true
    } else {
        prompt_yes_no(&format!(
            "A new release of raven-tui is available ({latest}; you have v{current}).\n\
             Update now? [y/N] "
        ))?
    };

    if !do_it {
        let mut st = load_state();
        st.skipped_version = Some(latest.clone());
        st.last_check_unix = now_unix();
        let _ = save_state(&st);
        eprintln!("==> Skipped update.");
        return Ok(false);
    }

    run_install_script()?;
    let mut st = load_state();
    st.last_check_unix = now_unix();
    st.skipped_version = None;
    let _ = save_state(&st);
    eprintln!(
        "==> Update installed. Exit and run `raven-tui` again (or open a new shell) to use {latest}."
    );
    Ok(true)
}

/// Once-per-day check for interactive TUI startup. Never fails the app hard.
pub fn maybe_daily_startup_check() {
    if std::env::var_os("RAVEN_NO_UPDATE_CHECK").is_some() {
        return;
    }
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return;
    }

    let mut st = load_state();
    let now = now_unix();
    if st.last_check_unix > 0 && now.saturating_sub(st.last_check_unix) < DAILY_SECS {
        return;
    }

    let current = current_version();
    let latest = match fetch_latest_tag() {
        Ok(t) => t,
        Err(e) => {
            // Soft-fail: offline, rate limit, etc.
            eprintln!("raven: update check skipped ({e})");
            st.last_check_unix = now;
            let _ = save_state(&st);
            return;
        }
    };

    st.last_check_unix = now;

    if !is_newer(&latest, current) {
        let _ = save_state(&st);
        return;
    }

    if st
        .skipped_version
        .as_deref()
        .is_some_and(|s| normalize_tag(s) == normalize_tag(&latest))
    {
        let _ = save_state(&st);
        return;
    }

    let yes = match prompt_yes_no(&format!(
        "A new release of raven-tui is available ({latest}; you have v{current}).\n\
         Would you like to update it? [y/N] "
    )) {
        Ok(v) => v,
        Err(_) => {
            let _ = save_state(&st);
            return;
        }
    };

    if !yes {
        st.skipped_version = Some(latest);
        let _ = save_state(&st);
        return;
    }

    if let Err(e) = run_install_script() {
        eprintln!("raven: update failed: {e}");
        let _ = save_state(&st);
        return;
    }

    st.skipped_version = None;
    let _ = save_state(&st);
    eprintln!(
        "raven: updated to {latest}. Exiting so you can restart with the new binary."
    );
    std::process::exit(0);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_and_compare() {
        assert_eq!(normalize_tag("v0.1.9"), "0.1.9");
        assert!(is_newer("v0.1.10", "0.1.9"));
        assert!(is_newer("0.2.0", "v0.1.99"));
        assert!(!is_newer("v0.1.9", "0.1.9"));
        assert!(!is_newer("v0.1.8", "0.1.9"));
    }
}
