//! Things oncue does on the user's behalf: open a session, send a reply,
//! allow a permission request, and post a desktop notification.
//!
//! Writes to agents go through the Paseo CLI only, and the TUI asks for a
//! confirmation before each one.

use crate::model::Session;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const CLI_TIMEOUT: Duration = Duration::from_secs(20);

/// What a key press asked for, waiting for `y` in the TUI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Reply {
        paseo_id: String,
        text: String,
    },
    Allow {
        paseo_id: String,
        request_id: String,
    },
}

impl Action {
    pub fn run(&self) -> anyhow::Result<()> {
        match self {
            Action::Reply { paseo_id, text } => {
                run_cli("paseo", &["send", paseo_id, "--prompt", text, "--no-wait"])
            }
            Action::Allow {
                paseo_id,
                request_id,
            } => run_cli("paseo", &["permit", "allow", paseo_id, request_id]),
        }
    }
}

/// Paseo's local server id, needed for agent deep links.
pub fn paseo_server_id() -> Option<String> {
    let home = std::env::var_os("PASEO_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| dirs::home_dir().map(|h| h.join(".paseo")))?;
    let id = std::fs::read_to_string(home.join("server-id")).ok()?;
    Some(id.trim().to_string()).filter(|s| !s.is_empty())
}

/// Where Enter takes a session: its Paseo agent, else its most recent PR.
pub fn open_target(s: &Session, paseo_server: Option<&str>) -> Option<String> {
    if let (Some(agent), Some(server)) = (&s.paseo_id, paseo_server) {
        return Some(format!("paseo://h/{server}/agent/{agent}"));
    }
    s.prs
        .last()
        .map(|p| p.url.clone())
        .filter(|u| !u.is_empty())
}

pub fn open(target: &str) -> anyhow::Result<()> {
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    run_cli(opener, &[target])
}

pub fn notify(title: &str, body: &str) {
    let result = if cfg!(target_os = "macos") {
        let script = format!(
            "display notification {} with title {}",
            applescript_string(body),
            applescript_string(title)
        );
        run_cli("osascript", &["-e", &script])
    } else {
        run_cli("notify-send", &[title, body])
    };
    // A missing notifier is not worth interrupting the user for.
    let _ = result;
}

fn applescript_string(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

fn run_cli(program: &str, args: &[&str]) -> anyhow::Result<()> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| anyhow::anyhow!("{program}: {e}"))?;
    let deadline = Instant::now() + CLI_TIMEOUT;
    while child.try_wait()?.is_none() {
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            anyhow::bail!("{program} timed out");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let out = child.wait_with_output()?;
    if out.status.success() {
        return Ok(());
    }
    let err = String::from_utf8_lossy(&out.stderr);
    anyhow::bail!("{program}: {}", crate::model::one_line(&err, 160))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::PrLink;

    #[test]
    fn paseo_agent_wins_over_pr_for_open() {
        let mut s = Session {
            paseo_id: Some("agent-1".into()),
            prs: vec![PrLink {
                url: "https://github.com/o/r/pull/1".into(),
                ..PrLink::default()
            }],
            ..Session::default()
        };
        assert_eq!(
            open_target(&s, Some("srv_1")).as_deref(),
            Some("paseo://h/srv_1/agent/agent-1")
        );
        assert_eq!(
            open_target(&s, None).as_deref(),
            Some("https://github.com/o/r/pull/1")
        );
        s.prs.clear();
        assert_eq!(open_target(&s, None), None);
    }

    #[test]
    fn applescript_strings_are_escaped() {
        assert_eq!(
            applescript_string(r#"say "hi" \ bye"#),
            r#""say \"hi\" \\ bye""#
        );
    }

    #[test]
    fn failing_command_reports_stderr() {
        let err = run_cli("sh", &["-c", "echo boom >&2; exit 1"]).unwrap_err();
        assert!(err.to_string().contains("boom"));
    }
}
