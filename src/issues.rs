//! Issue keys (`ENG-123`) found in sessions, and sessions that overlap.

use crate::model::{Host, IssueLink, Session, State, one_line};
use regex::Regex;
use std::collections::HashMap;

/// Finds issue keys in free text and branch names.
pub struct KeyMatcher {
    text: Regex,
    branch: Regex,
}

impl KeyMatcher {
    /// With known team keys only those match, in any case. Without them, text
    /// matches `ABC-123` in upper case and branches match the `team-123`
    /// segment that Linear puts in generated branch names.
    pub fn new(team_keys: &[String]) -> Self {
        let keys: Vec<String> = team_keys
            .iter()
            .filter(|k| !k.is_empty() && k.chars().all(|c| c.is_ascii_alphanumeric()))
            .map(|k| regex::escape(k))
            .collect();
        if keys.is_empty() {
            return Self {
                text: Regex::new(r"\b([A-Z][A-Z0-9]{1,9})-(\d{1,6})\b").unwrap(),
                branch: Regex::new(r"(?:^|/)([a-z][a-z0-9]{1,9})-(\d{1,6})(?:-|$)").unwrap(),
            };
        }
        let alt = keys.join("|");
        let re = Regex::new(&format!(r"(?i)\b({alt})-(\d{{1,6}})\b")).unwrap();
        Self {
            text: re.clone(),
            branch: re,
        }
    }

    pub fn in_text(&self, text: &str) -> Vec<String> {
        collect(&self.text, text)
    }

    pub fn in_branch(&self, branch: &str) -> Vec<String> {
        collect(&self.branch, branch)
    }
}

fn collect(re: &Regex, text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for c in re.captures_iter(text) {
        let key = format!("{}-{}", c[1].to_ascii_uppercase(), &c[2]);
        if !out.contains(&key) {
            out.push(key);
        }
    }
    out
}

/// Fill `issues` for every session, most reliable source first: the branch,
/// the branches and titles of its PRs, then its title.
pub fn link(sessions: &mut [Session], m: &KeyMatcher) {
    for s in sessions {
        let mut keys: Vec<String> = Vec::new();
        let mut add = |found: Vec<String>| {
            for k in found {
                if !keys.contains(&k) {
                    keys.push(k);
                }
            }
        };
        if let Some(b) = s.branch.as_deref().filter(|b| !is_trunk(b)) {
            add(m.in_branch(b));
        }
        for pr in s.prs.iter().rev() {
            if let Some(b) = &pr.branch {
                add(m.in_branch(b));
            }
            if let Some(t) = &pr.title {
                add(m.in_text(t));
            }
        }
        add(m.in_text(&s.title));
        s.issues = keys
            .into_iter()
            .map(|key| IssueLink {
                key,
                ..Default::default()
            })
            .collect();
    }
}

/// Live sessions that share their main issue or their branch with another live
/// session: two agents on the same work is how parallel sessions collide.
pub fn mark_overlaps(sessions: &mut [Session]) {
    let live = |s: &Session| s.host != Host::GitHub && s.state != State::Dormant;
    let mut groups: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, s) in sessions.iter().enumerate().filter(|(_, s)| live(s)) {
        if let Some(issue) = s.issues.first() {
            groups
                .entry(format!("issue:{}", issue.key))
                .or_default()
                .push(i);
        }
        if let Some(b) = s.branch.as_deref().filter(|b| !is_trunk(b)) {
            groups.entry(format!("branch:{b}")).or_default().push(i);
        }
    }
    let mut overlaps: Vec<Vec<String>> = vec![vec![]; sessions.len()];
    for members in groups.values().filter(|m| m.len() > 1) {
        for &i in members {
            for &j in members.iter().filter(|&&j| j != i) {
                let other = one_line(&sessions[j].title, 40);
                if !overlaps[i].contains(&other) {
                    overlaps[i].push(other);
                }
            }
        }
    }
    for (s, o) in sessions.iter_mut().zip(overlaps) {
        s.overlaps = o;
    }
}

fn is_trunk(branch: &str) -> bool {
    matches!(branch, "main" | "master" | "HEAD" | "develop" | "trunk")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::PrLink;

    fn session(key: &str, title: &str, branch: Option<&str>) -> Session {
        Session {
            key: key.into(),
            title: title.into(),
            branch: branch.map(str::to_string),
            ..Session::default()
        }
    }

    #[test]
    fn known_team_keys_match_any_case_and_nothing_else() {
        let m = KeyMatcher::new(&["ENG".into()]);
        assert_eq!(
            m.in_text("fix ENG-12 and eng-13, not UTF-8"),
            vec!["ENG-12", "ENG-13"]
        );
        assert_eq!(m.in_branch("feat/eng-887-push-admin"), vec!["ENG-887"]);
    }

    #[test]
    fn fallback_matches_upper_text_and_linear_branch_segments() {
        let m = KeyMatcher::new(&[]);
        assert_eq!(
            m.in_text("CAPABLE-553 진행해줘 (see ENG-1)"),
            vec!["CAPABLE-553", "ENG-1"]
        );
        assert!(m.in_text("utf-8 and sha-256").is_empty());
        assert_eq!(m.in_branch("fix/capable-945-chat-db"), vec!["CAPABLE-945"]);
        assert!(m.in_branch("feature/login-page").is_empty());
    }

    #[test]
    fn branch_wins_over_title_as_the_main_issue() {
        let m = KeyMatcher::new(&[]);
        let mut s = session("a", "Follow up on ENG-1", Some("feat/eng-2-x"));
        s.prs.push(PrLink {
            title: Some("feat: thing (ENG-3)".into()),
            ..PrLink::default()
        });
        let mut v = vec![s];
        link(&mut v, &m);
        let keys: Vec<_> = v[0].issues.iter().map(|i| i.key.as_str()).collect();
        assert_eq!(keys, vec!["ENG-2", "ENG-3", "ENG-1"]);
    }

    #[test]
    fn overlapping_live_sessions_are_marked() {
        let m = KeyMatcher::new(&[]);
        let mut v = vec![
            session("a", "ENG-7 build feed", None),
            session("b", "ENG-7 fix feed tests", None),
            session("c", "ENG-8 other", None),
            {
                let mut d = session("d", "ENG-7 old attempt", None);
                d.state = State::Dormant;
                d
            },
        ];
        link(&mut v, &m);
        mark_overlaps(&mut v);
        assert_eq!(v[0].overlaps, vec!["ENG-7 fix feed tests"]);
        assert_eq!(v[1].overlaps, vec!["ENG-7 build feed"]);
        assert!(v[2].overlaps.is_empty());
        assert!(v[3].overlaps.is_empty());
    }

    #[test]
    fn same_branch_overlaps_but_trunk_does_not() {
        let mut v = vec![
            session("a", "one", Some("feat/x")),
            session("b", "two", Some("feat/x")),
            session("c", "three", Some("main")),
            session("d", "four", Some("main")),
        ];
        mark_overlaps(&mut v);
        assert_eq!(v[0].overlaps, vec!["two"]);
        assert!(v[2].overlaps.is_empty());
    }
}
