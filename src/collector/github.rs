//! Pull request state from GitHub, through the user's `gh` CLI auth.
//!
//! Two sources feed one cache: the PRs that sessions linked (`pr-link`), and
//! the user's own open PRs. Fetching runs on a background thread in the TUI so
//! the local refresh never waits on the network.

use crate::model::{Host, PrLink, PrState, Session, State, one_line};
use serde_json::Value;
use std::collections::{BTreeSet, HashMap};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const GH_TIMEOUT: Duration = Duration::from_secs(25);
/// PRs per GraphQL request. Larger requests time out on GitHub's side.
const CHUNK: usize = 12;
const FIELDS: &str = "number title url state isDraft merged mergeable mergeStateStatus \
    reviewDecision headRefName updatedAt repository { nameWithOwner } \
    commits(last: 1) { nodes { commit { statusCheckRollup { state } } } } \
    reviewThreads(last: 30) { nodes { isResolved } }";

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PrKey {
    pub repo: String,
    pub number: u64,
}

#[derive(Debug, Clone)]
pub struct Pr {
    pub repo: String,
    pub number: u64,
    pub url: String,
    pub title: String,
    pub branch: String,
    pub state: PrState,
    pub updated_ms: Option<u64>,
    /// Raw `mergeStateStatus`, e.g. `BLOCKED`, for the detail pane.
    pub merge_state: String,
}

impl Pr {
    fn key(&self) -> PrKey {
        PrKey {
            repo: self.repo.clone(),
            number: self.number,
        }
    }
}

#[derive(Default)]
struct Cache {
    prs: HashMap<PrKey, Pr>,
    /// The user's open PRs from the last search.
    mine: BTreeSet<PrKey>,
    fetched: Option<Instant>,
    error: Option<String>,
}

#[derive(Clone)]
pub struct GitHub {
    cache: Arc<Mutex<Cache>>,
    wanted: Arc<Mutex<BTreeSet<PrKey>>>,
    /// Search for the user's own open PRs; `None` to skip.
    mine: Option<MySearch>,
}

/// The user's open PRs, limited to recent activity and optionally to owners.
#[derive(Clone)]
pub struct MySearch {
    pub stale_after_days: u64,
    pub owners: Vec<String>,
}

impl MySearch {
    fn query(&self) -> String {
        let since = chrono::Utc::now() - chrono::Duration::days(self.stale_after_days as i64);
        let mut q = format!(
            "is:pr is:open author:@me archived:false updated:>={}",
            since.format("%Y-%m-%d")
        );
        for owner in &self.owners {
            q.push_str(&format!(" user:{owner}"));
        }
        q
    }
}

impl GitHub {
    pub fn new(mine: Option<MySearch>) -> Self {
        Self {
            cache: Arc::default(),
            wanted: Arc::default(),
            mine,
        }
    }

    /// Refresh everything every `interval`, and fetch newly linked PRs within a
    /// couple of seconds of their appearance.
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

    pub fn want(&self, keys: impl IntoIterator<Item = PrKey>) {
        self.wanted.lock().unwrap().extend(keys);
    }

    pub fn refresh_all(&self) {
        let wanted: Vec<PrKey> = self.wanted.lock().unwrap().iter().cloned().collect();
        let result = fetch_all(&wanted, self.mine.as_ref());
        let mut cache = self.cache.lock().unwrap();
        cache.fetched = Some(Instant::now());
        match result {
            Ok((prs, mine)) => {
                cache.error = None;
                if let Some(mine) = mine {
                    cache.mine = mine;
                }
                for pr in prs {
                    cache.prs.insert(pr.key(), pr);
                }
            }
            Err(e) => cache.error = Some(e.to_string()),
        }
    }

    fn refresh_missing(&self) {
        let missing: Vec<PrKey> = {
            let cache = self.cache.lock().unwrap();
            self.wanted
                .lock()
                .unwrap()
                .iter()
                .filter(|k| !cache.prs.contains_key(k))
                .cloned()
                .collect()
        };
        if missing.is_empty() {
            return;
        }
        if let Ok(prs) = fetch_by_key(&missing) {
            let mut cache = self.cache.lock().unwrap();
            for pr in prs {
                cache.prs.insert(pr.key(), pr);
            }
        }
    }

    /// Attach PR state to sessions, raise idle sessions whose PR needs the
    /// user, and add rows for the user's open PRs that no live session owns.
    pub fn apply(
        &self,
        sessions: &mut Vec<Session>,
        stale_after_ms: u64,
        now: u64,
        warnings: &mut Vec<String>,
    ) {
        let cache = self.cache.lock().unwrap();
        if let Some(e) = &cache.error {
            warnings.push(format!("github: {}", one_line(e, 120)));
        }
        let mut owned = BTreeSet::new();
        for s in sessions.iter_mut() {
            for link in &mut s.prs {
                let key = PrKey {
                    repo: link.repo.clone(),
                    number: link.number,
                };
                if let Some(pr) = cache.prs.get(&key) {
                    link.state = Some(pr.state);
                    link.title = Some(pr.title.clone());
                    link.branch = Some(pr.branch.clone());
                }
                owned.insert(key);
            }
            raise_for_prs(s, &cache.prs, stale_after_ms, now);
        }
        for key in &cache.mine {
            if owned.contains(key) {
                continue;
            }
            if let Some(row) = cache
                .prs
                .get(key)
                .and_then(|pr| pr_row(pr, stale_after_ms, now))
            {
                sessions.push(row);
            }
        }
    }
}

/// An idle session whose open PR needs the user moves up to the PR's reason.
/// Working sessions are left alone: the agent is presumably on it.
fn raise_for_prs(s: &mut Session, prs: &HashMap<PrKey, Pr>, stale_after_ms: u64, now: u64) {
    if s.state == State::Working {
        return;
    }
    let best = s
        .prs
        .iter()
        .filter_map(|l| {
            let pr = prs.get(&PrKey {
                repo: l.repo.clone(),
                number: l.number,
            })?;
            let stale = pr
                .updated_ms
                .is_some_and(|t| now.saturating_sub(t) > stale_after_ms);
            if stale { None } else { pr.state.attention() }
        })
        .min();
    let Some(attention) = best else { return };
    if s.attention().is_none_or(|c| attention < c) {
        s.state = State::NeedsYou(attention);
    }
}

/// A row for an open PR with no live session. Only fresh PRs that need the
/// user or are running checks get one; the rest are noise.
fn pr_row(pr: &Pr, stale_after_ms: u64, now: u64) -> Option<Session> {
    let stale = pr
        .updated_ms
        .is_some_and(|t| now.saturating_sub(t) > stale_after_ms);
    let state = match (pr.state.attention(), pr.state) {
        _ if stale => return None,
        (Some(a), _) => State::NeedsYou(a),
        (None, PrState::Pending) => State::Working,
        _ => return None,
    };
    let link = PrLink {
        repo: pr.repo.clone(),
        number: pr.number,
        url: pr.url.clone(),
        state: Some(pr.state),
        title: Some(pr.title.clone()),
        branch: Some(pr.branch.clone()),
    };
    Some(Session {
        key: format!("gh:{}", pr.url),
        agent: "github".into(),
        host: Host::GitHub,
        title: one_line(&pr.title, 200),
        cwd: Default::default(),
        project: pr.repo.rsplit('/').next().unwrap_or(&pr.repo).to_string(),
        branch: Some(pr.branch.clone()),
        state,
        since_ms: pr.updated_ms,
        detail: Some(format!("{} · {}", pr.url, pr.merge_state)),
        options: vec![],
        activity: (pr.state == PrState::Pending).then(|| "checks".to_string()),
        resets_at_ms: None,
        pid: None,
        session_id: None,
        paseo_id: None,
        prs: vec![link],
        ..Default::default()
    })
}

type Fetched = (Vec<Pr>, Option<BTreeSet<PrKey>>);

fn fetch_all(wanted: &[PrKey], mine: Option<&MySearch>) -> anyhow::Result<Fetched> {
    let (mut prs, mine) = if let Some(search) = mine {
        let ids = search_pr_ids(&search.query())?;
        let prs = fetch_by_id(&ids)?;
        let mine = prs.iter().map(Pr::key).collect();
        (prs, Some(mine))
    } else {
        (vec![], None)
    };
    let have: BTreeSet<PrKey> = prs.iter().map(Pr::key).collect();
    let rest: Vec<PrKey> = wanted
        .iter()
        .filter(|k| !have.contains(k))
        .cloned()
        .collect();
    prs.extend(fetch_by_key(&rest)?);
    Ok((prs, mine))
}

fn search_pr_ids(search: &str) -> anyhow::Result<Vec<String>> {
    let q = format!(
        "query {{ s: search(query: {}, type: ISSUE, first: 100) {{ nodes {{ ... on PullRequest {{ id }} }} }} }}",
        serde_json::to_string(search)?
    );
    let data = graphql(&q)?;
    Ok(data
        .pointer("/s/nodes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|n| n.get("id").and_then(Value::as_str).map(str::to_string))
        .collect())
}

fn fetch_by_id(ids: &[String]) -> anyhow::Result<Vec<Pr>> {
    let queries: Vec<String> = ids
        .chunks(CHUNK)
        .map(|chunk| {
            let list = chunk
                .iter()
                .map(|id| format!("\"{id}\""))
                .collect::<Vec<_>>()
                .join(",");
            format!("query {{ nodes(ids: [{list}]) {{ ... on PullRequest {{ {FIELDS} }} }} }}")
        })
        .collect();
    run_chunks(queries, |data| {
        data.get("nodes")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    })
}

fn fetch_by_key(keys: &[PrKey]) -> anyhow::Result<Vec<Pr>> {
    let queries: Vec<String> = keys
        .chunks(CHUNK)
        .map(|chunk| {
            let parts: Vec<String> = chunk
                .iter()
                .enumerate()
                .filter_map(|(i, k)| {
                    let (owner, name) = k.repo.split_once('/')?;
                    Some(format!(
                        "p{i}: repository(owner: \"{owner}\", name: \"{name}\") {{ pullRequest(number: {}) {{ {FIELDS} }} }}",
                        k.number
                    ))
                })
                .collect();
            format!("query {{ {} }}", parts.join(" "))
        })
        .collect();
    run_chunks(queries, |data| {
        data.as_object()
            .into_iter()
            .flat_map(|o| o.values())
            .filter_map(|v| v.get("pullRequest").cloned())
            .collect()
    })
}

/// Run chunk queries in parallel and parse every PR node they return.
fn run_chunks(
    queries: Vec<String>,
    nodes: impl Fn(&Value) -> Vec<Value> + Sync,
) -> anyhow::Result<Vec<Pr>> {
    if queries.is_empty() {
        return Ok(vec![]);
    }
    let results: Vec<anyhow::Result<Value>> = std::thread::scope(|scope| {
        let handles: Vec<_> = queries
            .iter()
            .map(|q| scope.spawn(move || graphql(q)))
            .collect();
        handles
            .into_iter()
            .map(|h| {
                h.join()
                    .unwrap_or_else(|_| Err(anyhow::anyhow!("gh thread panicked")))
            })
            .collect()
    });
    let mut prs = Vec::new();
    let mut last_err = None;
    for r in results {
        match r {
            Ok(data) => prs.extend(nodes(&data).iter().filter_map(parse_pr)),
            Err(e) => last_err = Some(e),
        }
    }
    match (prs.is_empty(), last_err) {
        (true, Some(e)) => Err(e),
        _ => Ok(prs),
    }
}

fn graphql(query: &str) -> anyhow::Result<Value> {
    let mut child = Command::new("gh")
        .args(["api", "graphql", "-f"])
        .arg(format!("query={query}"))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| anyhow::anyhow!("gh not available: {e}"))?;
    let deadline = Instant::now() + GH_TIMEOUT;
    while child.try_wait()?.is_none() {
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            anyhow::bail!("gh timed out");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let out = child.wait_with_output()?;
    let body: Value = serde_json::from_slice(&out.stdout).unwrap_or(Value::Null);
    if let Some(data) = body.get("data").filter(|d| !d.is_null()) {
        return Ok(data.clone());
    }
    let msg = body
        .pointer("/errors/0/message")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| String::from_utf8_lossy(&out.stderr).trim().to_string());
    anyhow::bail!("{msg}")
}

fn parse_pr(v: &Value) -> Option<Pr> {
    let s = |p: &str| v.pointer(p).and_then(Value::as_str);
    let unresolved = v
        .pointer("/reviewThreads/nodes")
        .and_then(Value::as_array)
        .map_or(0, |n| {
            n.iter()
                .filter(|t| t.get("isResolved") == Some(&Value::Bool(false)))
                .count()
        });
    let state = classify(&Raw {
        state: s("/state").unwrap_or(""),
        merged: v.get("merged").and_then(Value::as_bool).unwrap_or(false),
        draft: v.get("isDraft").and_then(Value::as_bool).unwrap_or(false),
        mergeable: s("/mergeable").unwrap_or(""),
        merge_state: s("/mergeStateStatus").unwrap_or(""),
        review: s("/reviewDecision").unwrap_or(""),
        checks: s("/commits/nodes/0/commit/statusCheckRollup/state"),
        unresolved,
    });
    Some(Pr {
        repo: s("/repository/nameWithOwner")?.to_string(),
        number: v.get("number")?.as_u64()?,
        url: s("/url")?.to_string(),
        title: s("/title").unwrap_or("").to_string(),
        branch: s("/headRefName").unwrap_or("").to_string(),
        state,
        updated_ms: s("/updatedAt")
            .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
            .map(|t| t.timestamp_millis().max(0) as u64),
        merge_state: s("/mergeStateStatus").unwrap_or("").to_string(),
    })
}

struct Raw<'a> {
    state: &'a str,
    merged: bool,
    draft: bool,
    mergeable: &'a str,
    merge_state: &'a str,
    review: &'a str,
    checks: Option<&'a str>,
    unresolved: usize,
}

fn classify(r: &Raw) -> PrState {
    if r.merged || r.state == "MERGED" {
        return PrState::Merged;
    }
    if r.state == "CLOSED" {
        return PrState::Closed;
    }
    if r.draft {
        return PrState::Draft;
    }
    if r.mergeable == "CONFLICTING" || r.merge_state == "DIRTY" {
        return PrState::Conflict;
    }
    match r.checks {
        Some("FAILURE" | "ERROR") => return PrState::ChecksFailed,
        Some("PENDING" | "EXPECTED") => return PrState::Pending,
        _ => {}
    }
    if r.review == "CHANGES_REQUESTED" {
        return PrState::ChangesRequested;
    }
    if r.unresolved > 0 {
        return PrState::Unresolved;
    }
    match r.merge_state {
        "BEHIND" => PrState::Behind,
        // GitHub is still computing mergeability.
        "UNKNOWN" | "" => PrState::Pending,
        // CLEAN, HAS_HOOKS, UNSTABLE, and BLOCKED with green checks: branch
        // rules may still require an approval, but that is the owner's call.
        _ => PrState::Ready,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Attention;

    fn raw() -> Raw<'static> {
        Raw {
            state: "OPEN",
            merged: false,
            draft: false,
            mergeable: "MERGEABLE",
            merge_state: "CLEAN",
            review: "",
            checks: Some("SUCCESS"),
            unresolved: 0,
        }
    }

    #[test]
    fn classifies_in_owner_priority_order() {
        assert_eq!(classify(&raw()), PrState::Ready);
        assert_eq!(
            classify(&Raw {
                merge_state: "BLOCKED",
                ..raw()
            }),
            PrState::Ready
        );
        assert_eq!(
            classify(&Raw {
                checks: Some("PENDING"),
                ..raw()
            }),
            PrState::Pending
        );
        assert_eq!(
            classify(&Raw {
                checks: Some("FAILURE"),
                ..raw()
            }),
            PrState::ChecksFailed
        );
        assert_eq!(
            classify(&Raw {
                unresolved: 1,
                merge_state: "BLOCKED",
                ..raw()
            }),
            PrState::Unresolved
        );
        assert_eq!(
            classify(&Raw {
                mergeable: "CONFLICTING",
                checks: Some("FAILURE"),
                ..raw()
            }),
            PrState::Conflict
        );
        assert_eq!(
            classify(&Raw {
                merge_state: "BEHIND",
                ..raw()
            }),
            PrState::Behind
        );
        assert_eq!(
            classify(&Raw {
                draft: true,
                checks: Some("FAILURE"),
                ..raw()
            }),
            PrState::Draft
        );
        assert_eq!(
            classify(&Raw {
                merged: true,
                state: "MERGED",
                ..raw()
            }),
            PrState::Merged
        );
    }

    #[test]
    fn parses_graphql_node() {
        let v = serde_json::json!({
            "number": 7, "title": "Add feed", "url": "https://github.com/o/r/pull/7",
            "state": "OPEN", "isDraft": false, "merged": false, "mergeable": "MERGEABLE",
            "mergeStateStatus": "BLOCKED", "reviewDecision": null, "headRefName": "feat/feed",
            "updatedAt": "2026-09-28T10:00:00Z", "repository": {"nameWithOwner": "o/r"},
            "commits": {"nodes": [{"commit": {"statusCheckRollup": {"state": "SUCCESS"}}}]},
            "reviewThreads": {"nodes": [{"isResolved": true}, {"isResolved": false}]}
        });
        let pr = parse_pr(&v).unwrap();
        assert_eq!(pr.state, PrState::Unresolved);
        assert_eq!(pr.repo, "o/r");
        assert_eq!(pr.branch, "feat/feed");
    }

    fn pr(state: PrState) -> Pr {
        Pr {
            repo: "o/r".into(),
            number: 7,
            url: "https://github.com/o/r/pull/7".into(),
            title: "Add feed".into(),
            branch: "feat/feed".into(),
            state,
            updated_ms: Some(1_000),
            merge_state: "CLEAN".into(),
        }
    }

    fn session(state: State) -> Session {
        let mut s = pr_row(&pr(PrState::Pending), u64::MAX, 0).unwrap();
        s.key = "claude:s".into();
        s.host = Host::Terminal;
        s.state = state;
        s.detail = Some("PR is up.".into());
        s
    }

    #[test]
    fn idle_session_with_ready_pr_moves_to_merge() {
        let prs = HashMap::from([(
            PrKey {
                repo: "o/r".into(),
                number: 7,
            },
            pr(PrState::Ready),
        )]);
        let mut s = session(State::NeedsYou(Attention::Idle));
        raise_for_prs(&mut s, &prs, u64::MAX, 0);
        assert_eq!(s.state, State::NeedsYou(Attention::Merge));
    }

    #[test]
    fn working_session_is_not_raised() {
        let prs = HashMap::from([(
            PrKey {
                repo: "o/r".into(),
                number: 7,
            },
            pr(PrState::ChecksFailed),
        )]);
        let mut s = session(State::Working);
        raise_for_prs(&mut s, &prs, u64::MAX, 0);
        assert_eq!(s.state, State::Working);
    }

    #[test]
    fn question_outranks_ready_pr() {
        let prs = HashMap::from([(
            PrKey {
                repo: "o/r".into(),
                number: 7,
            },
            pr(PrState::Ready),
        )]);
        let mut s = session(State::NeedsYou(Attention::Question));
        raise_for_prs(&mut s, &prs, u64::MAX, 0);
        assert_eq!(s.state, State::NeedsYou(Attention::Question));
    }

    #[test]
    fn dormant_session_is_raised_only_for_a_fresh_pr() {
        let prs = HashMap::from([(
            PrKey {
                repo: "o/r".into(),
                number: 7,
            },
            pr(PrState::ChecksFailed),
        )]);
        let mut s = session(State::Dormant);
        raise_for_prs(&mut s, &prs, 10, 1_000_000);
        assert_eq!(s.state, State::Dormant);
        raise_for_prs(&mut s, &prs, u64::MAX, 0);
        assert_eq!(s.state, State::NeedsYou(Attention::Blocked));
    }

    #[test]
    fn only_fresh_actionable_or_running_prs_get_rows() {
        let state = |st, stale, now| pr_row(&pr(st), stale, now).map(|s| s.state);
        assert_eq!(state(PrState::Ready, 10, 1_000_000), None);
        assert_eq!(state(PrState::Draft, u64::MAX, 0), None);
        assert_eq!(
            state(PrState::Ready, u64::MAX, 0),
            Some(State::NeedsYou(Attention::Merge))
        );
        assert_eq!(state(PrState::Pending, u64::MAX, 0), Some(State::Working));
    }

    #[test]
    fn search_limits_to_recent_prs_and_owners() {
        let q = MySearch {
            stale_after_days: 3,
            owners: vec!["acme".into(), "me".into()],
        }
        .query();
        assert!(q.starts_with("is:pr is:open author:@me archived:false updated:>="));
        assert!(q.ends_with(" user:acme user:me"));
    }
}
