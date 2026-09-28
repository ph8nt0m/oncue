pub mod claude;
pub mod github;
pub mod paseo;

use crate::config::Config;
use crate::model::{Attention, Snapshot, State, now_ms};
use std::path::PathBuf;
use std::time::Duration;

pub struct Collector {
    claude_roots: Vec<PathBuf>,
    paseo_home: Option<PathBuf>,
    /// Background permit polling in the TUI; `None` fetches inline.
    permit_watcher: Option<paseo::PermitWatcher>,
    started: std::time::Instant,
    dormant_after_ms: u64,
    github: Option<github::GitHub>,
    /// Fetch GitHub state inline on every collect (for --once/--json).
    github_blocking: bool,
    pr_stale_ms: u64,
}

impl Collector {
    /// `background` starts a thread that keeps GitHub state fresh; otherwise
    /// each `collect` fetches it inline.
    pub fn new(config: &Config, background: bool) -> Self {
        let github = config.github.enabled.then(|| {
            let mine = config.github.my_prs.then(|| github::MySearch {
                stale_after_days: config.github.stale_after_days,
                owners: config.github.owners.clone(),
            });
            let gh = github::GitHub::new(mine);
            if background {
                gh.spawn(Duration::from_secs(config.github.interval_secs.max(15)));
            }
            gh
        });
        Self {
            github,
            github_blocking: !background,
            pr_stale_ms: config.github.stale_after_days * 86_400_000,
            claude_roots: claude::discover_roots(&config.claude_config_dirs),
            paseo_home: if config.paseo { paseo::home() } else { None },
            permit_watcher: (config.paseo && background).then(paseo::PermitWatcher::spawn),
            started: std::time::Instant::now(),
            dormant_after_ms: config.dormant_after_minutes * 60_000,
        }
    }

    pub fn collect(&self) -> Snapshot {
        let now = now_ms();
        let mut warnings = Vec::new();
        let mut sessions = claude::collect(&self.claude_roots, &mut warnings);

        if let Some(home) = &self.paseo_home {
            let permits = match &self.permit_watcher {
                Some(w) => w.latest(),
                None => paseo::permits(),
            };
            // Give the first background poll time before calling it a failure.
            let warming_up = self.permit_watcher.is_some() && self.started.elapsed().as_secs() < 15;
            let permits = permits.unwrap_or_else(|| {
                if !warming_up {
                    warnings.push("paseo: permission requests unavailable".into());
                }
                Default::default()
            });
            paseo::merge(&mut sessions, paseo::agents(home), &permits);
        }

        for s in &mut sessions {
            // Blocking requests never go dormant; finished turns do.
            let stale = s
                .since_ms
                .is_some_and(|t| now.saturating_sub(t) > self.dormant_after_ms);
            if stale
                && matches!(
                    s.state,
                    State::NeedsYou(Attention::Unread | Attention::Idle)
                )
            {
                s.state = State::Dormant;
            }
        }

        if let Some(gh) = &self.github {
            gh.want(sessions.iter().flat_map(|s| {
                s.prs.iter().map(|p| github::PrKey {
                    repo: p.repo.clone(),
                    number: p.number,
                })
            }));
            if self.github_blocking {
                gh.refresh_all();
            }
            gh.apply(&mut sessions, self.pr_stale_ms, now, &mut warnings);
        }

        let mut snapshot = Snapshot {
            generated_at_ms: now,
            sessions,
            warnings,
        };
        snapshot.sort();
        snapshot
    }
}
