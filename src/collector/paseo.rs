//! Paseo-managed agents.
//!
//! Agent records live in `~/.paseo/agents/<workspace>/<id>.json` and already
//! carry an attention flag (`requiresAttention` + `attentionReason`). Pending
//! permission requests are only held by the daemon, so they come from
//! `paseo permit ls --json`.

use crate::git;
use crate::model::{Attention, Host, Session, State, is_trivial_title, one_line, project_name};
use serde::Deserialize;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const PERMIT_TIMEOUT: Duration = Duration::from_secs(4);

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Agent {
    pub id: String,
    #[serde(default)]
    pub provider: String,
    pub cwd: PathBuf,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub last_status: Option<String>,
    #[serde(default)]
    pub requires_attention: bool,
    #[serde(default)]
    pub attention_reason: Option<String>,
    #[serde(default)]
    pub attention_timestamp: Option<String>,
    #[serde(default)]
    pub last_activity_at: Option<String>,
    #[serde(default)]
    pub archived_at: Option<String>,
    #[serde(default)]
    pub internal: bool,
    #[serde(default)]
    pub persistence: Option<Persistence>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Persistence {
    #[serde(default)]
    pub session_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Permit {
    pub agent_id: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
}

pub fn home() -> Option<PathBuf> {
    std::env::var_os("PASEO_HOME")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|h| h.join(".paseo")))
        .filter(|p| p.join("agents").is_dir())
}

/// Active (not archived, not internal) agents.
pub fn agents(home: &Path) -> Vec<Agent> {
    let Ok(workspaces) = fs::read_dir(home.join("agents")) else {
        return vec![];
    };
    workspaces
        .flatten()
        .filter_map(|w| fs::read_dir(w.path()).ok())
        .flat_map(|files| files.flatten())
        .filter(|f| f.path().extension().is_some_and(|e| e == "json"))
        .filter_map(|f| serde_json::from_slice::<Agent>(&fs::read(f.path()).ok()?).ok())
        .filter(|a| a.archived_at.is_none() && !a.internal)
        .collect()
}

/// Pending permission requests keyed by agent id. `None` when the CLI is
/// missing or the daemon did not answer in time.
pub fn permits() -> Option<HashMap<String, Vec<Permit>>> {
    let mut child = Command::new("paseo")
        .args(["permit", "ls", "--json"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + PERMIT_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    let out = child.wait_with_output().ok()?;
    if !out.status.success() {
        return None;
    }
    let list: Vec<Permit> = serde_json::from_slice(&out.stdout).ok()?;
    let mut map: HashMap<String, Vec<Permit>> = HashMap::new();
    for p in list {
        map.entry(p.agent_id.clone()).or_default().push(p);
    }
    Some(map)
}

/// Fold Paseo's view into the sessions found on disk. A Paseo agent that owns a
/// live Claude session enriches it; any other agent becomes its own row.
pub fn merge(
    sessions: &mut Vec<Session>,
    agents: Vec<Agent>,
    permits: &HashMap<String, Vec<Permit>>,
) {
    for agent in agents {
        let pending = permits.get(&agent.id).map(Vec::as_slice).unwrap_or(&[]);
        let session_id = agent
            .persistence
            .as_ref()
            .and_then(|p| p.session_id.clone());
        let existing = session_id.as_deref().and_then(|id| {
            sessions
                .iter_mut()
                .find(|s| s.session_id.as_deref() == Some(id))
        });

        match existing {
            Some(s) => {
                s.host = Host::Paseo;
                s.paseo_id = Some(agent.id.clone());
                if let Some(t) = agent.title.as_deref().filter(|t| !is_trivial_title(t)) {
                    s.title = one_line(t, 200);
                }
                apply_attention(s, &agent, pending);
            }
            None => {
                let mut s = standalone(&agent);
                apply_attention(&mut s, &agent, pending);
                sessions.push(s);
            }
        }
    }
}

fn standalone(agent: &Agent) -> Session {
    let state = match agent.last_status.as_deref() {
        Some("running") => State::Working,
        Some("idle") => State::NeedsYou(Attention::Unread),
        _ => State::Dormant,
    };
    Session {
        key: format!("paseo:{}", agent.id),
        agent: agent.provider.clone(),
        host: Host::Paseo,
        title: one_line(agent.title.as_deref().unwrap_or(""), 200),
        project: project_name(&agent.cwd),
        branch: git::branch(&agent.cwd),
        cwd: agent.cwd.clone(),
        state,
        since_ms: agent.last_activity_at.as_deref().and_then(parse_ms),
        detail: None,
        options: vec![],
        activity: None,
        resets_at_ms: None,
        pid: None,
        session_id: agent
            .persistence
            .as_ref()
            .and_then(|p| p.session_id.clone()),
        paseo_id: Some(agent.id.clone()),
        prs: vec![],
    }
}

fn apply_attention(s: &mut Session, agent: &Agent, pending: &[Permit]) {
    // A question shows up as a permission request too; the transcript already
    // has the better description, so keep it.
    if let Some(p) = pending.iter().find(|p| p.name != "AskUserQuestion") {
        if s.state != State::NeedsYou(Attention::Question) {
            s.state = State::NeedsYou(Attention::Permission);
            s.detail = Some(match &p.description {
                Some(d) => format!("{}: {}", p.name, one_line(d, 400)),
                None => p.name.clone(),
            });
        }
        return;
    }
    if pending.iter().any(|p| p.name == "AskUserQuestion") && s.attention().is_none() {
        s.state = State::NeedsYou(Attention::Question);
        return;
    }
    let flagged_at = agent.attention_timestamp.as_deref().and_then(parse_ms);
    match (agent.requires_attention, agent.attention_reason.as_deref()) {
        (true, Some("error")) => {
            s.state = State::NeedsYou(Attention::Error);
            s.since_ms = flagged_at.or(s.since_ms);
        }
        (true, _) => {
            if matches!(s.state, State::Dormant | State::NeedsYou(Attention::Idle)) {
                s.state = State::NeedsYou(Attention::Unread);
            }
            s.since_ms = flagged_at.or(s.since_ms);
        }
        // Paseo clears the flag once the user opens the agent.
        (false, _) => {
            if s.state == State::NeedsYou(Attention::Unread) {
                s.state = State::NeedsYou(Attention::Idle);
            }
        }
    }
}

fn parse_ms(ts: &str) -> Option<u64> {
    chrono::DateTime::parse_from_rfc3339(ts)
        .ok()
        .map(|t| t.timestamp_millis().max(0) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(json: &str) -> Agent {
        serde_json::from_str(json).unwrap()
    }

    fn claude_session(id: &str, state: State) -> Session {
        Session {
            key: format!("claude:{id}"),
            agent: "claude".into(),
            host: Host::Terminal,
            title: "t".into(),
            cwd: "/tmp".into(),
            project: "tmp".into(),
            branch: None,
            state,
            since_ms: Some(1),
            detail: None,
            options: vec![],
            activity: None,
            resets_at_ms: None,
            pid: Some(1),
            session_id: Some(id.into()),
            paseo_id: None,
            prs: vec![],
        }
    }

    #[test]
    fn seen_unanswered_agent_becomes_idle() {
        let mut sessions = vec![claude_session("s1", State::NeedsYou(Attention::Unread))];
        let a = agent(
            r#"{"id":"p1","cwd":"/tmp","title":"Fix CI","requiresAttention":false,"persistence":{"sessionId":"s1"}}"#,
        );
        merge(&mut sessions, vec![a], &HashMap::new());
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].state, State::NeedsYou(Attention::Idle));
        assert_eq!(sessions[0].title, "Fix CI");
        assert_eq!(sessions[0].host, Host::Paseo);
    }

    #[test]
    fn pending_permission_overrides_working() {
        let mut sessions = vec![claude_session("s1", State::Working)];
        let a = agent(r#"{"id":"p1","cwd":"/tmp","persistence":{"sessionId":"s1"}}"#);
        let permits = HashMap::from([(
            "p1".to_string(),
            vec![Permit {
                agent_id: "p1".into(),
                name: "Bash".into(),
                description: Some("terraform apply".into()),
            }],
        )]);
        merge(&mut sessions, vec![a], &permits);
        assert_eq!(sessions[0].state, State::NeedsYou(Attention::Permission));
        assert_eq!(sessions[0].detail.as_deref(), Some("Bash: terraform apply"));
    }

    #[test]
    fn agent_without_live_session_is_its_own_row() {
        let mut sessions = vec![];
        let a = agent(
            r#"{"id":"p2","provider":"codex","cwd":"/tmp/x","lastStatus":"idle","requiresAttention":true,"attentionReason":"error","attentionTimestamp":"2026-09-28T10:00:00Z"}"#,
        );
        merge(&mut sessions, vec![a], &HashMap::new());
        assert_eq!(sessions[0].agent, "codex");
        assert_eq!(sessions[0].state, State::NeedsYou(Attention::Error));
        assert_eq!(sessions[0].since_ms, parse_ms("2026-09-28T10:00:00Z"));
    }

    #[test]
    fn closed_agent_without_flag_is_dormant() {
        let mut sessions = vec![];
        let a = agent(r#"{"id":"p3","cwd":"/tmp","lastStatus":"closed"}"#);
        merge(&mut sessions, vec![a], &HashMap::new());
        assert_eq!(sessions[0].state, State::Dormant);
    }
}
