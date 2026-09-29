use serde::Serialize;
use std::path::PathBuf;

/// Why a session is waiting on the user. Ordered by urgency: earlier variants
/// sort first in the queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Attention {
    /// The agent asked a question (e.g. `AskUserQuestion`) and is blocked on the answer.
    Question,
    /// A tool call is waiting for approval.
    Permission,
    /// A plan is waiting for approval (`ExitPlanMode`).
    Plan,
    /// A pull request passed checks and only needs the merge decision.
    Merge,
    /// The agent stopped with an error.
    Error,
    /// A pull request is stuck: failing checks, conflicts, requested changes,
    /// unresolved threads, or behind its base.
    Blocked,
    /// The account hit a usage limit; the session can resume after the reset.
    Limited,
    /// The agent finished its turn and nobody has looked at the result yet.
    Unread,
    /// The agent finished its turn; the result was seen but not answered.
    Idle,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", content = "reason", rename_all = "snake_case")]
pub enum State {
    #[default]
    Working,
    NeedsYou(Attention),
    /// Waiting on the user for longer than the dormant threshold, or closed.
    Dormant,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Host {
    /// A CLI process started directly in a terminal.
    #[default]
    Terminal,
    /// An agent managed by the Paseo daemon.
    Paseo,
    /// A pull request with no live session behind it.
    GitHub,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Session {
    /// Stable identity across refreshes, e.g. `claude:<session id>`.
    pub key: String,
    pub agent: String,
    pub host: Host,
    pub title: String,
    pub cwd: PathBuf,
    pub project: String,
    pub branch: Option<String>,
    pub state: State,
    /// When the session entered its current state (Unix ms).
    pub since_ms: Option<u64>,
    /// What the user needs to read: the question, the tool call, or the last message.
    pub detail: Option<String>,
    /// Answer choices when the session asked a question.
    pub options: Vec<String>,
    /// What a working session is doing right now.
    pub activity: Option<String>,
    /// When a usage limit resets (Unix ms), for `Limited` sessions.
    pub resets_at_ms: Option<u64>,
    pub pid: Option<u32>,
    pub session_id: Option<String>,
    pub paseo_id: Option<String>,
    /// Pending Paseo permission request, for `Permission` sessions.
    pub permit_id: Option<String>,
    /// Pull requests the session opened or referenced, newest last.
    pub prs: Vec<PrLink>,
    /// Issue keys found in the branch, PRs, and title; the first is the main one.
    pub issues: Vec<IssueLink>,
    /// Titles of other live sessions on the same main issue or branch.
    pub overlaps: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct IssueLink {
    pub key: String,
    /// Filled in by the Linear collector.
    pub title: Option<String>,
    pub url: Option<String>,
    /// Workflow state name as shown in the tracker, e.g. "In Progress".
    pub state: Option<String>,
    pub state_kind: Option<IssueStateKind>,
}

/// Linear's workflow state types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IssueStateKind {
    Triage,
    Backlog,
    Unstarted,
    Started,
    Completed,
    Canceled,
}

impl IssueStateKind {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "triage" => Self::Triage,
            "backlog" => Self::Backlog,
            "unstarted" => Self::Unstarted,
            "started" => Self::Started,
            "completed" => Self::Completed,
            "canceled" => Self::Canceled,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct PrLink {
    pub repo: String,
    pub number: u64,
    pub url: String,
    /// Filled in by the GitHub collector.
    pub state: Option<PrState>,
    pub title: Option<String>,
    pub branch: Option<String>,
}

impl PrLink {
    /// `repo#123` with the owner dropped.
    pub fn short(&self) -> String {
        let repo = self.repo.rsplit('/').next().unwrap_or(&self.repo);
        format!("{repo}#{}", self.number)
    }
}

/// Where a pull request stands, from the owner's point of view. Checked in this
/// order, so a PR with a conflict and failing checks reports the conflict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PrState {
    Merged,
    Closed,
    Draft,
    Conflict,
    ChecksFailed,
    ChangesRequested,
    Unresolved,
    Pending,
    Behind,
    /// Checks passed and nothing is open; only the merge decision is left.
    Ready,
}

impl PrState {
    pub fn attention(self) -> Option<Attention> {
        match self {
            PrState::Ready => Some(Attention::Merge),
            PrState::Conflict
            | PrState::ChecksFailed
            | PrState::ChangesRequested
            | PrState::Unresolved
            | PrState::Behind => Some(Attention::Blocked),
            PrState::Merged | PrState::Closed | PrState::Draft | PrState::Pending => None,
        }
    }
}

impl Session {
    pub fn attention(&self) -> Option<Attention> {
        match self.state {
            State::NeedsYou(a) => Some(a),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Snapshot {
    pub generated_at_ms: u64,
    pub sessions: Vec<Session>,
    /// Usage limits per account, from `usage_command`.
    pub usage: Vec<crate::collector::usage::Account>,
    /// Non-fatal collector problems, shown in the footer.
    pub warnings: Vec<String>,
}

impl Snapshot {
    /// Queue order: most urgent reason first, then the longest wait first.
    pub fn sort(&mut self) {
        self.sessions.sort_by(|a, b| {
            rank(&a.state)
                .cmp(&rank(&b.state))
                .then(
                    a.since_ms
                        .unwrap_or(u64::MAX)
                        .cmp(&b.since_ms.unwrap_or(u64::MAX)),
                )
                .then(a.key.cmp(&b.key))
        });
    }

    pub fn count(&self, pred: impl Fn(&State) -> bool) -> usize {
        self.sessions.iter().filter(|s| pred(&s.state)).count()
    }
}

fn rank(state: &State) -> (u8, u8) {
    match state {
        State::NeedsYou(a) => (0, *a as u8),
        State::Working => (1, 0),
        State::Dormant => (2, 0),
    }
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Last path component of `cwd`, used as a short project label.
pub fn project_name(cwd: &std::path::Path) -> String {
    cwd.file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| cwd.display().to_string())
}

/// Prompts like "continue" or "ok" say nothing about the task.
pub fn is_trivial_title(text: &str) -> bool {
    text.trim().chars().count() <= 5
}

/// Collapse whitespace and cut to `max` characters.
pub fn one_line(text: &str, max: usize) -> String {
    let flat: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        flat
    } else {
        let cut: String = flat.chars().take(max.saturating_sub(1)).collect();
        format!("{cut}…")
    }
}
