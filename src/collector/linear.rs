//! Issue state from Linear.
//!
//! The API key comes from an environment variable or a user-configured command
//! (for example a keychain lookup). It is kept in memory only.

use crate::model::{IssueStateKind, Session};
use serde_json::Value;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const ENDPOINT: &str = "https://api.linear.app/graphql";
const CHUNK: usize = 25;
const KEY_COMMAND_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone)]
pub struct Issue {
    pub title: String,
    pub url: String,
    pub state: String,
    pub kind: Option<IssueStateKind>,
}

#[derive(Default)]
struct Cache {
    issues: HashMap<String, Issue>,
    /// Keys Linear does not know, so they are not asked for again until the
    /// next full refresh.
    unknown: HashSet<String>,
    teams: Option<Vec<String>>,
    fetched: Option<Instant>,
    error: Option<String>,
}

#[derive(Clone)]
pub struct Linear {
    api_key: Arc<str>,
    cache: Arc<Mutex<Cache>>,
    wanted: Arc<Mutex<BTreeSet<String>>>,
    agent: ureq::Agent,
}

/// The API key from `env` or, failing that, the stdout of `command`.
pub fn api_key(env: &str, command: &str) -> Option<String> {
    if let Some(v) = std::env::var(env).ok().filter(|v| !v.trim().is_empty()) {
        return Some(v.trim().to_string());
    }
    if command.trim().is_empty() {
        return None;
    }
    let mut child = Command::new("sh")
        .args(["-c", command])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + KEY_COMMAND_TIMEOUT;
    while child.try_wait().ok()?.is_none() {
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let out = child.wait_with_output().ok()?;
    let key = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (out.status.success() && !key.is_empty()).then_some(key)
}

impl Linear {
    pub fn new(api_key: String) -> Self {
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(20)))
            .build()
            .into();
        Self {
            api_key: api_key.into(),
            cache: Arc::default(),
            wanted: Arc::default(),
            agent,
        }
    }

    pub fn spawn(&self, interval: Duration) {
        let this = self.clone();
        std::thread::spawn(move || {
            loop {
                let due = this
                    .cache
                    .lock()
                    .unwrap()
                    .fetched
                    .is_none_or(|t| t.elapsed() >= interval);
                if due {
                    this.refresh_all();
                } else {
                    this.refresh_missing();
                }
                std::thread::sleep(Duration::from_secs(2));
            }
        });
    }

    /// Team keys from the workspace, once known.
    pub fn team_keys(&self) -> Option<Vec<String>> {
        self.cache.lock().unwrap().teams.clone()
    }

    pub fn want(&self, keys: impl IntoIterator<Item = String>) {
        self.wanted.lock().unwrap().extend(keys);
    }

    pub fn refresh_all(&self) {
        let teams = if self.team_keys().is_none() {
            Some(self.fetch_teams())
        } else {
            None
        };
        let wanted: Vec<String> = self.wanted.lock().unwrap().iter().cloned().collect();
        let result = self.fetch_issues(&wanted);
        let mut cache = self.cache.lock().unwrap();
        cache.fetched = Some(Instant::now());
        cache.error = None;
        match teams {
            Some(Ok(t)) => cache.teams = Some(t),
            Some(Err(e)) => cache.error = Some(e.to_string()),
            None => {}
        }
        match result {
            Ok(found) => {
                cache.unknown = wanted
                    .iter()
                    .filter(|k| !found.contains_key(*k))
                    .cloned()
                    .collect();
                cache.issues.extend(found);
            }
            Err(e) => cache.error = Some(e.to_string()),
        }
    }

    fn refresh_missing(&self) {
        let missing: Vec<String> = {
            let cache = self.cache.lock().unwrap();
            self.wanted
                .lock()
                .unwrap()
                .iter()
                .filter(|k| !cache.issues.contains_key(*k) && !cache.unknown.contains(*k))
                .cloned()
                .collect()
        };
        if missing.is_empty() {
            return;
        }
        if let Ok(found) = self.fetch_issues(&missing) {
            let mut cache = self.cache.lock().unwrap();
            cache
                .unknown
                .extend(missing.iter().filter(|k| !found.contains_key(*k)).cloned());
            cache.issues.extend(found);
        }
    }

    pub fn apply(&self, sessions: &mut [Session], warnings: &mut Vec<String>) {
        let cache = self.cache.lock().unwrap();
        if let Some(e) = &cache.error {
            warnings.push(format!("linear: {}", crate::model::one_line(e, 120)));
        }
        for link in sessions.iter_mut().flat_map(|s| s.issues.iter_mut()) {
            if let Some(issue) = cache.issues.get(&link.key) {
                link.title = Some(issue.title.clone());
                link.url = Some(issue.url.clone());
                link.state = Some(issue.state.clone());
                link.state_kind = issue.kind;
            }
        }
        drop(cache);
        // Keys Linear does not know are probably not issues (e.g. `UTF-8`).
        let unknown = self.cache.lock().unwrap().unknown.clone();
        for s in sessions.iter_mut() {
            s.issues.retain(|i| !unknown.contains(&i.key));
        }
    }

    fn fetch_teams(&self) -> anyhow::Result<Vec<String>> {
        let data = self.graphql("query { teams(first: 250) { nodes { key } } }")?;
        Ok(data
            .pointer("/teams/nodes")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|t| t.get("key").and_then(Value::as_str).map(str::to_string))
            .collect())
    }

    fn fetch_issues(&self, keys: &[String]) -> anyhow::Result<HashMap<String, Issue>> {
        let mut found = HashMap::new();
        for chunk in keys.chunks(CHUNK) {
            let parts: Vec<String> = chunk
                .iter()
                .enumerate()
                .filter(|(_, k)| is_issue_key(k))
                .map(|(i, k)| {
                    format!(
                        "i{i}: issue(id: \"{k}\") {{ identifier title url state {{ name type }} }}"
                    )
                })
                .collect();
            if parts.is_empty() {
                continue;
            }
            // Unknown keys come back as errors next to the data for the rest.
            let data = self.graphql(&format!("query {{ {} }}", parts.join(" ")))?;
            found.extend(parse_issues(&data));
        }
        Ok(found)
    }

    fn graphql(&self, query: &str) -> anyhow::Result<Value> {
        let body = serde_json::json!({ "query": query });
        let mut resp = self
            .agent
            .post(ENDPOINT)
            .header("Authorization", &*self.api_key)
            .config()
            .http_status_as_error(false)
            .build()
            .send_json(&body)?;
        let status = resp.status();
        let v: Value = resp.body_mut().read_json().unwrap_or(Value::Null);
        match v.get("data").filter(|d| d.is_object()) {
            Some(d) => Ok(d.clone()),
            None => {
                let msg = v
                    .pointer("/errors/0/message")
                    .and_then(Value::as_str)
                    .unwrap_or("request failed");
                anyhow::bail!("{status}: {msg}")
            }
        }
    }
}

/// `TEAM-123`, the only shape interpolated into a query.
fn is_issue_key(k: &str) -> bool {
    match k.split_once('-') {
        Some((team, n)) => {
            !team.is_empty()
                && team.chars().all(|c| c.is_ascii_alphanumeric())
                && !n.is_empty()
                && n.chars().all(|c| c.is_ascii_digit())
        }
        None => false,
    }
}

fn parse_issues(data: &Value) -> HashMap<String, Issue> {
    data.as_object()
        .into_iter()
        .flat_map(|o| o.values())
        .filter_map(|v| {
            let s = |p: &str| v.pointer(p).and_then(Value::as_str);
            Some((
                s("/identifier")?.to_string(),
                Issue {
                    title: s("/title").unwrap_or("").to_string(),
                    url: s("/url").unwrap_or("").to_string(),
                    state: s("/state/name").unwrap_or("").to_string(),
                    kind: s("/state/type").and_then(IssueStateKind::parse),
                },
            ))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_team_number_keys_reach_the_query() {
        assert!(is_issue_key("ENG-12"));
        assert!(!is_issue_key("ENG-12\") { x } y: issue(id: \"1"));
        assert!(!is_issue_key("ENG"));
        assert!(!is_issue_key("-12"));
    }

    #[test]
    fn parses_aliased_issues_and_skips_nulls() {
        let data = serde_json::json!({
            "i0": {"identifier": "ENG-1", "title": "Feed", "url": "https://linear.app/x/issue/ENG-1",
                   "state": {"name": "In Progress", "type": "started"}},
            "i1": null
        });
        let issues = parse_issues(&data);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues["ENG-1"].kind, Some(IssueStateKind::Started));
        assert_eq!(issues["ENG-1"].state, "In Progress");
    }

    #[test]
    fn api_key_prefers_env_then_command() {
        assert_eq!(
            api_key("ONCUE_TEST_NO_SUCH_VAR", "echo lin_api_x").as_deref(),
            Some("lin_api_x")
        );
        assert_eq!(api_key("ONCUE_TEST_NO_SUCH_VAR", "exit 1"), None);
        assert_eq!(api_key("ONCUE_TEST_NO_SUCH_VAR", ""), None);
    }
}
