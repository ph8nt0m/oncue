//! Claude Code sessions.
//!
//! Live sessions are discovered from `<root>/sessions/<pid>.json`, which Claude
//! Code keeps up to date with `status: busy | idle`. Why a session is waiting
//! comes from the tail of its transcript at
//! `<root>/projects/<cwd slug>/<session id>.jsonl`.

use crate::git;
use crate::model::{
    Attention, Host, PrLink, Session, State, is_trivial_title, one_line, project_name,
};
use serde::Deserialize;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

const TAIL_BYTES: u64 = 512 * 1024;
const HEAD_BYTES: u64 = 64 * 1024;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionFile {
    pid: u32,
    session_id: String,
    cwd: PathBuf,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    name_source: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    status_updated_at: Option<u64>,
    #[serde(default)]
    updated_at: Option<u64>,
}

/// Config roots to scan: `~/.claude`, `~/.claude-*`, `~/.claude-profiles/*`,
/// `$CLAUDE_CONFIG_DIR`, and any extra roots from the config file. Only
/// directories with a `sessions/` subdirectory are kept.
pub fn discover_roots(extra: &[PathBuf]) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(home) = dirs::home_dir() {
        roots.push(home.join(".claude"));
        if let Ok(entries) = fs::read_dir(&home) {
            for e in entries.flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                if name.starts_with(".claude-") {
                    roots.push(e.path());
                }
            }
        }
        if let Ok(entries) = fs::read_dir(home.join(".claude-profiles")) {
            roots.extend(entries.flatten().map(|e| e.path()));
        }
    }
    if let Some(dir) = std::env::var_os("CLAUDE_CONFIG_DIR") {
        roots.push(PathBuf::from(dir));
    }
    roots.extend(extra.iter().cloned());

    let mut seen = HashSet::new();
    roots
        .into_iter()
        .filter(|r| r.join("sessions").is_dir())
        .filter(|r| seen.insert(fs::canonicalize(r).unwrap_or_else(|_| r.clone())))
        .collect()
}

pub fn collect(roots: &[PathBuf], warnings: &mut Vec<String>) -> Vec<Session> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for root in roots {
        let Ok(entries) = fs::read_dir(root.join("sessions")) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_none_or(|e| e != "json") {
                continue;
            }
            let file: SessionFile = match fs::read(&path).map(|b| serde_json::from_slice(&b)) {
                Ok(Ok(f)) => f,
                _ => continue,
            };
            if !pid_alive(file.pid) || !seen.insert(file.session_id.clone()) {
                continue;
            }
            match build_session(root, file) {
                Ok(s) => out.push(s),
                Err(e) => warnings.push(format!("claude: {e}")),
            }
        }
    }
    out
}

fn build_session(root: &Path, file: SessionFile) -> anyhow::Result<Session> {
    let transcript = transcript_path(root, &file.cwd, &file.session_id);
    let tail = match &transcript {
        Some(p) => read_tail(p)?,
        None => Tail::default(),
    };
    let busy = file.status.as_deref() == Some("busy");
    let transcript_mtime = transcript.as_deref().and_then(mtime_ms);

    let c = classify(&tail, busy);
    let since_ms = match c.state {
        // Nothing is written to the transcript while a question is open, so the
        // last write is when the agent started waiting.
        State::NeedsYou(Attention::Question | Attention::Plan) => transcript_mtime,
        State::NeedsYou(_) => file
            .status_updated_at
            .or(file.updated_at)
            .or(transcript_mtime),
        _ => None,
    };

    let title = match (&file.name, file.name_source.as_deref()) {
        (Some(n), Some(src)) if src != "derived" => n.clone(),
        _ => transcript
            .as_deref()
            .and_then(first_prompt)
            .or(tail.last_prompt.clone().filter(|p| !is_trivial_title(p)))
            .or(file.name.clone())
            .unwrap_or_default(),
    };

    Ok(Session {
        key: format!("claude:{}", file.session_id),
        agent: "claude".into(),
        host: Host::Terminal,
        title: one_line(&title, 200),
        project: project_name(&file.cwd),
        branch: git::branch(&file.cwd),
        cwd: file.cwd,
        state: c.state,
        since_ms,
        detail: c.detail,
        options: c.options,
        activity: c.activity,
        resets_at_ms: c.resets_at_ms,
        pid: Some(file.pid),
        session_id: Some(file.session_id),
        paseo_id: None,
        prs: tail.prs,
        ..Default::default()
    })
}

#[derive(Debug, Default)]
struct Classified {
    state: State,
    detail: Option<String>,
    options: Vec<String>,
    activity: Option<String>,
    resets_at_ms: Option<u64>,
}

fn classify(tail: &Tail, busy: bool) -> Classified {
    if let Some(tool) = tail.pending.last() {
        match tool.name.as_str() {
            "AskUserQuestion" => {
                let (text, options) = describe_question(&tool.input);
                return Classified {
                    state: State::NeedsYou(Attention::Question),
                    detail: Some(text),
                    options,
                    ..Default::default()
                };
            }
            "ExitPlanMode" => {
                let plan = tool.input.get("plan").and_then(Value::as_str).unwrap_or("");
                return Classified {
                    state: State::NeedsYou(Attention::Plan),
                    detail: Some(one_line(plan, 2000)),
                    ..Default::default()
                };
            }
            _ if busy => {
                return Classified {
                    state: State::Working,
                    activity: Some(describe_tool(tool)),
                    ..Default::default()
                };
            }
            _ => {}
        }
    }
    if busy {
        let activity = if tail.pending.is_empty() {
            "thinking"
        } else {
            "tool"
        };
        return Classified {
            state: State::Working,
            activity: Some(activity.into()),
            ..Default::default()
        };
    }
    if let Some(err) = &tail.api_error {
        return Classified {
            state: State::NeedsYou(if err.rate_limited {
                Attention::Limited
            } else {
                Attention::Error
            }),
            detail: Some(one_line(&err.message, 400)),
            resets_at_ms: err.resets_at_ms,
            ..Default::default()
        };
    }
    let detail = if tail.interrupted {
        Some("[interrupted]".to_string())
    } else {
        tail.last_text.as_deref().map(|t| one_line(t, 2000))
    };
    Classified {
        state: State::NeedsYou(Attention::Unread),
        detail,
        ..Default::default()
    }
}

fn describe_question(input: &Value) -> (String, Vec<String>) {
    let questions = input.get("questions").and_then(Value::as_array);
    let Some(questions) = questions else {
        return (String::new(), vec![]);
    };
    let text = questions
        .iter()
        .filter_map(|q| q.get("question").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join(" / ");
    let options = questions
        .first()
        .and_then(|q| q.get("options"))
        .and_then(Value::as_array)
        .map(|opts| {
            opts.iter()
                .filter_map(|o| o.get("label").and_then(Value::as_str).map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    (text, options)
}

fn describe_tool(tool: &PendingTool) -> String {
    let arg = [
        "command",
        "description",
        "file_path",
        "pattern",
        "url",
        "prompt",
    ]
    .iter()
    .find_map(|k| tool.input.get(*k).and_then(Value::as_str));
    match arg {
        Some(a) => format!("{} {}", tool.name, one_line(a, 80)),
        None => tool.name.clone(),
    }
}

#[derive(Debug, Clone)]
struct PendingTool {
    id: String,
    name: String,
    input: Value,
}

#[derive(Debug)]
struct ApiError {
    rate_limited: bool,
    message: String,
    resets_at_ms: Option<u64>,
}

#[derive(Debug, Default)]
struct Tail {
    pending: Vec<PendingTool>,
    /// Set when the last assistant turn was an API error rather than a reply.
    api_error: Option<ApiError>,
    last_text: Option<String>,
    last_prompt: Option<String>,
    interrupted: bool,
    prs: Vec<PrLink>,
}

fn read_tail(path: &Path) -> anyhow::Result<Tail> {
    let mut file = File::open(path)?;
    let len = file.metadata()?.len();
    let start = len.saturating_sub(TAIL_BYTES);
    file.seek(SeekFrom::Start(start))?;
    let mut buf = Vec::with_capacity((len - start) as usize);
    file.read_to_end(&mut buf)?;
    let text = String::from_utf8_lossy(&buf);
    let mut lines = text.lines();
    if start > 0 {
        lines.next(); // Partial line.
    }
    Ok(parse_tail(lines))
}

fn parse_tail<'a>(lines: impl Iterator<Item = &'a str>) -> Tail {
    let mut tail = Tail::default();
    let mut pending: Vec<PendingTool> = Vec::new();
    let mut prs: HashMap<String, PrLink> = HashMap::new();
    let mut pr_order: Vec<String> = Vec::new();

    for line in lines {
        let Ok(entry) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let kind = entry.get("type").and_then(Value::as_str).unwrap_or("");
        match kind {
            "pr-link" => {
                if let Some(pr) = parse_pr_link(&entry) {
                    let k = pr.url.clone();
                    if !prs.contains_key(&k) {
                        pr_order.push(k.clone());
                    }
                    prs.insert(k, pr);
                }
                continue;
            }
            "last-prompt" => {
                if let Some(p) = entry.get("lastPrompt").and_then(Value::as_str) {
                    tail.last_prompt = Some(p.to_string());
                }
                continue;
            }
            "user" | "assistant" => {}
            _ => continue,
        }
        if entry.get("isSidechain").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        let content = entry.pointer("/message/content");
        match kind {
            "assistant" => {
                tail.interrupted = false;
                if entry.get("isApiErrorMessage").and_then(Value::as_bool) == Some(true) {
                    tail.api_error = Some(parse_api_error(&entry));
                    continue;
                }
                tail.api_error = None;
                for block in content.and_then(Value::as_array).into_iter().flatten() {
                    match block.get("type").and_then(Value::as_str) {
                        Some("text") => {
                            if let Some(t) = block.get("text").and_then(Value::as_str) {
                                if !t.trim().is_empty() {
                                    tail.last_text = Some(t.to_string());
                                }
                            }
                        }
                        Some("tool_use") => pending.push(PendingTool {
                            id: block.get("id").and_then(Value::as_str).unwrap_or("").into(),
                            name: block
                                .get("name")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .into(),
                            input: block.get("input").cloned().unwrap_or(Value::Null),
                        }),
                        _ => {}
                    }
                }
            }
            _ => {
                let blocks: Vec<&Value> = match content {
                    Some(Value::Array(a)) => a.iter().collect(),
                    _ => vec![],
                };
                for block in &blocks {
                    if block.get("type").and_then(Value::as_str) == Some("tool_result") {
                        let id = block
                            .get("tool_use_id")
                            .and_then(Value::as_str)
                            .unwrap_or("");
                        pending.retain(|p| p.id != id);
                    }
                }
                let text = match content {
                    Some(Value::String(s)) => Some(s.as_str()),
                    _ => blocks
                        .iter()
                        .find(|b| b.get("type").and_then(Value::as_str) == Some("text"))
                        .and_then(|b| b.get("text").and_then(Value::as_str)),
                };
                if let Some(t) = text {
                    if t.starts_with("[Request interrupted") {
                        tail.interrupted = true;
                        pending.clear();
                    }
                }
            }
        }
    }
    tail.pending = pending;
    tail.prs = pr_order
        .into_iter()
        .filter_map(|k| prs.remove(&k))
        .collect();
    tail
}

fn parse_api_error(entry: &Value) -> ApiError {
    let message = entry
        .pointer("/message/content")
        .and_then(Value::as_array)
        .and_then(|blocks| {
            blocks
                .iter()
                .find_map(|b| b.get("text").and_then(Value::as_str))
        })
        .unwrap_or("API error")
        .to_string();
    ApiError {
        rate_limited: entry.get("error").and_then(Value::as_str) == Some("rate_limit"),
        message,
        resets_at_ms: entry
            .pointer("/quotaLimits/resetsAt")
            .and_then(Value::as_u64)
            .map(|s| s * 1000),
    }
}

fn parse_pr_link(entry: &Value) -> Option<PrLink> {
    Some(PrLink {
        repo: entry.get("prRepository")?.as_str()?.to_string(),
        number: entry.get("prNumber")?.as_u64()?,
        url: entry.get("prUrl")?.as_str()?.to_string(),
        ..Default::default()
    })
}

/// First prompt the user typed, skipping command wrappers and injected context.
fn first_prompt(path: &Path) -> Option<String> {
    let mut buf = Vec::new();
    File::open(path)
        .ok()?
        .take(HEAD_BYTES)
        .read_to_end(&mut buf)
        .ok()?;
    let text = String::from_utf8_lossy(&buf);
    for line in text.lines() {
        let Ok(entry) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if entry.get("type").and_then(Value::as_str) != Some("user")
            || entry.get("isMeta").and_then(Value::as_bool) == Some(true)
            || entry.get("isSidechain").and_then(Value::as_bool) == Some(true)
        {
            continue;
        }
        let text = match entry.pointer("/message/content") {
            Some(Value::String(s)) => Some(s.clone()),
            Some(Value::Array(a)) => a
                .iter()
                .find(|b| b.get("type").and_then(Value::as_str) == Some("text"))
                .and_then(|b| b.get("text").and_then(Value::as_str))
                .map(str::to_string),
            _ => None,
        };
        let Some(t) = text else { continue };
        let t = t.trim();
        // A Paseo handoff starts with a summary that names the original task.
        if let Some(source) = t
            .strip_prefix("<chat-history-summary>")
            .and_then(|rest| rest.lines().find_map(|l| l.strip_prefix("Source agent: ")))
            .filter(|s| !is_trivial_title(s))
        {
            return Some(source.trim().to_string());
        }
        let injected = t.starts_with('<')
            || t.starts_with("Caveat:")
            || t.starts_with("Base directory for this skill");
        if !injected && !is_trivial_title(t) {
            return Some(t.to_string());
        }
    }
    None
}

/// Claude Code stores transcripts under a slug of the cwd in which every
/// non-alphanumeric character becomes `-`.
fn slug(cwd: &Path) -> String {
    cwd.to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

fn transcript_path(root: &Path, cwd: &Path, session_id: &str) -> Option<PathBuf> {
    let file = format!("{session_id}.jsonl");
    let direct = root.join("projects").join(slug(cwd)).join(&file);
    if direct.is_file() {
        return Some(direct);
    }
    fs::read_dir(root.join("projects"))
        .ok()?
        .flatten()
        .map(|e| e.path().join(&file))
        .find(|p| p.is_file())
}

fn mtime_ms(path: &Path) -> Option<u64> {
    let t = fs::metadata(path).ok()?.modified().ok()?;
    Some(t.duration_since(std::time::UNIX_EPOCH).ok()?.as_millis() as u64)
}

#[cfg(unix)]
fn pid_alive(pid: u32) -> bool {
    // SAFETY: signal 0 only checks for existence and permission.
    let rc = unsafe { libc::kill(pid as libc::pid_t, 0) };
    rc == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(not(unix))]
fn pid_alive(_pid: u32) -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tail(lines: &[&str]) -> Tail {
        parse_tail(lines.iter().copied())
    }

    const ASK: &str = r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Two decisions left."},{"type":"tool_use","id":"t1","name":"AskUserQuestion","input":{"questions":[{"question":"Which host?","options":[{"label":"A"},{"label":"B"}]}]}}]}}"#;
    const ASK_ANSWERED: &str = r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"A"}]}}"#;
    const BASH: &str = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t2","name":"Bash","input":{"command":"cargo test"}}]}}"#;
    const DONE: &str =
        r#"{"type":"assistant","message":{"content":[{"type":"text","text":"PR #12 is up."}]}}"#;

    #[test]
    fn open_question_needs_you_even_while_busy() {
        let t = tail(&[ASK]);
        let c = classify(&t, true);
        assert_eq!(c.state, State::NeedsYou(Attention::Question));
        assert_eq!(c.detail.as_deref(), Some("Which host?"));
        assert_eq!(c.options, vec!["A", "B"]);
    }

    #[test]
    fn answered_question_then_tool_is_working() {
        let t = tail(&[ASK, ASK_ANSWERED, BASH]);
        let c = classify(&t, true);
        assert_eq!(c.state, State::Working);
        assert_eq!(c.activity.as_deref(), Some("Bash cargo test"));
    }

    #[test]
    fn idle_after_report_is_unread_with_last_message() {
        let t = tail(&[
            BASH,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t2"}]}}"#,
            DONE,
        ]);
        let c = classify(&t, false);
        assert_eq!(c.state, State::NeedsYou(Attention::Unread));
        assert_eq!(c.detail.as_deref(), Some("PR #12 is up."));
    }

    #[test]
    fn interrupt_clears_pending_tools() {
        let t = tail(&[
            BASH,
            r#"{"type":"user","message":{"content":[{"type":"text","text":"[Request interrupted by user]"}]}}"#,
        ]);
        assert!(t.pending.is_empty());
        assert_eq!(classify(&t, false).detail.as_deref(), Some("[interrupted]"));
    }

    #[test]
    fn sidechain_entries_are_ignored() {
        let t = tail(&[
            DONE,
            r#"{"type":"assistant","isSidechain":true,"message":{"content":[{"type":"tool_use","id":"s","name":"AskUserQuestion","input":{}}]}}"#,
        ]);
        assert!(t.pending.is_empty());
    }

    #[test]
    fn pr_links_are_deduplicated_in_order() {
        let a = r#"{"type":"pr-link","prNumber":1,"prUrl":"https://github.com/o/r/pull/1","prRepository":"o/r"}"#;
        let b = r#"{"type":"pr-link","prNumber":2,"prUrl":"https://github.com/o/r/pull/2","prRepository":"o/r"}"#;
        let t = tail(&[a, b, a]);
        assert_eq!(
            t.prs.iter().map(|p| p.number).collect::<Vec<_>>(),
            vec![1, 2]
        );
    }

    const LIMIT: &str = r#"{"type":"assistant","isApiErrorMessage":true,"error":"rate_limit","quotaLimits":{"resetsAt":1790416800,"rateLimitType":"five_hour"},"message":{"content":[{"type":"text","text":"You've hit your session limit · resets 8pm"}]}}"#;

    #[test]
    fn rate_limit_error_is_limited_with_reset() {
        let c = classify(&tail(&[DONE, LIMIT]), false);
        assert_eq!(c.state, State::NeedsYou(Attention::Limited));
        assert_eq!(c.resets_at_ms, Some(1_790_416_800_000));
        assert_eq!(
            c.detail.as_deref(),
            Some("You've hit your session limit · resets 8pm")
        );
    }

    #[test]
    fn reply_after_limit_clears_it() {
        let c = classify(&tail(&[LIMIT, DONE]), false);
        assert_eq!(c.state, State::NeedsYou(Attention::Unread));
    }

    #[test]
    fn handoff_title_comes_from_source_agent() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("t.jsonl");
        let summary = serde_json::json!({"type":"user","message":{"content":"<chat-history-summary>\nChat history from a previous Paseo agent.\nSource agent: Design the thread feed\n"}});
        let cont = serde_json::json!({"type":"user","message":{"content":"ok go"}});
        fs::write(&p, format!("{summary}\n{cont}\n")).unwrap();
        assert_eq!(first_prompt(&p).as_deref(), Some("Design the thread feed"));
    }

    #[test]
    fn slug_matches_claude_code() {
        assert_eq!(
            slug(Path::new("/Users/a/.paseo/wt-1")),
            "-Users-a--paseo-wt-1"
        );
    }
}
