//! Per-account usage limits from a user-configured command.
//!
//! oncue never touches agent credentials. `usage_command` prints a JSON array,
//! one object per account:
//!
//! ```json
//! [{"label": "Work", "provider": "claude",
//!   "five_hour": {"used": 0.42, "reset": 1790666400, "status": "allowed"},
//!   "seven_day": {"used": 0.81, "reset": 1790802000},
//!   "error": null}]
//! ```
//!
//! `used` is a fraction (0-1), `reset` a Unix time in seconds. Every field but
//! `label` is optional; unknown fields are ignored.

use serde::{Deserialize, Serialize};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Account {
    /// Display name; falls back to `name` when missing.
    #[serde(default)]
    pub label: String,
    #[serde(default, skip_serializing)]
    name: String,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default, deserialize_with = "window")]
    pub five_hour: Option<Window>,
    #[serde(default, deserialize_with = "window")]
    pub seven_day: Option<Window>,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Window {
    pub used: f64,
    #[serde(default)]
    pub reset: Option<f64>,
    #[serde(default)]
    pub status: Option<String>,
}

impl Window {
    pub fn reset_ms(&self) -> Option<u64> {
        self.reset.map(|s| (s * 1000.0) as u64)
    }

    /// The provider has started refusing or warning for this window.
    pub fn is_limited(&self) -> bool {
        matches!(self.status.as_deref(), Some("rejected" | "blocked")) || self.used >= 1.0
    }
}

/// An empty object (`{}`) means "no such window", not an error.
fn window<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<Window>, D::Error> {
    let v = Option::<serde_json::Value>::deserialize(d)?;
    Ok(v.and_then(|v| serde_json::from_value(v).ok()))
}

pub fn parse(json: &[u8]) -> anyhow::Result<Vec<Account>> {
    let mut accounts: Vec<Account> = serde_json::from_slice(json)?;
    for a in &mut accounts {
        if a.label.is_empty() {
            a.label = std::mem::take(&mut a.name);
        }
    }
    accounts.retain(|a| !a.label.is_empty());
    Ok(accounts)
}

pub fn run(command: &str) -> anyhow::Result<Vec<Account>> {
    let mut child = Command::new("sh")
        .args(["-c", command])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let deadline = Instant::now() + COMMAND_TIMEOUT;
    while child.try_wait()?.is_none() {
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            anyhow::bail!("usage_command timed out");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let out = child.wait_with_output()?;
    if !out.status.success() {
        anyhow::bail!("usage_command exited with {}", out.status);
    }
    parse(&out.stdout)
}

#[derive(Clone, Default)]
pub struct UsageWatcher {
    latest: Arc<Mutex<(Vec<Account>, Option<String>)>>,
}

impl UsageWatcher {
    pub fn spawn(command: String, interval: Duration) -> Self {
        let w = Self::default();
        let this = w.clone();
        std::thread::spawn(move || {
            loop {
                let result = run(&command);
                {
                    let mut latest = this.latest.lock().unwrap();
                    match result {
                        Ok(accounts) => *latest = (accounts, None),
                        // Keep the last good numbers; just report the failure.
                        Err(e) => latest.1 = Some(e.to_string()),
                    }
                }
                std::thread::sleep(interval);
            }
        });
        w
    }

    pub fn latest(&self) -> (Vec<Account>, Option<String>) {
        self.latest.lock().unwrap().clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_cc_usage_style_output() {
        let json = br#"[
          {"name":"b","label":"Staff.1","provider":"claude","plan":"max",
           "five_hour":{"used":0.02,"reset":1790666400.0,"status":"allowed"},
           "seven_day":{"used":0.81,"reset":1790802000.0,"status":"allowed_warning"}},
          {"name":"codex-x","label":"Staff","provider":"codex","five_hour":{},
           "seven_day":{"used":0.42,"reset":1791046725,"status":"allowed"}},
          {"name":"only-name"}
        ]"#;
        let a = parse(json).unwrap();
        assert_eq!(a[0].label, "Staff.1");
        assert_eq!(
            a[0].seven_day.as_ref().unwrap().reset_ms(),
            Some(1_790_802_000_000)
        );
        assert!(a[1].five_hour.is_none());
        assert_eq!(a[2].label, "only-name");
    }

    #[test]
    fn rejected_or_full_window_is_limited() {
        let w = |used, status: &str| Window {
            used,
            reset: None,
            status: Some(status.into()),
        };
        assert!(w(0.3, "rejected").is_limited());
        assert!(w(1.0, "allowed").is_limited());
        assert!(!w(0.9, "allowed_warning").is_limited());
    }

    #[test]
    fn command_failures_are_errors() {
        assert!(run("exit 3").is_err());
        assert!(run("echo not-json").is_err());
        assert_eq!(run(r#"echo '[{"label":"a"}]'"#).unwrap().len(), 1);
    }
}
