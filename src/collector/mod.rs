pub mod claude;
pub mod paseo;

use crate::config::Config;
use crate::model::{Attention, Snapshot, State, now_ms};
use std::path::PathBuf;

pub struct Collector {
    claude_roots: Vec<PathBuf>,
    paseo_home: Option<PathBuf>,
    dormant_after_ms: u64,
}

impl Collector {
    pub fn new(config: &Config) -> Self {
        Self {
            claude_roots: claude::discover_roots(&config.claude_config_dirs),
            paseo_home: if config.paseo { paseo::home() } else { None },
            dormant_after_ms: config.dormant_after_minutes * 60_000,
        }
    }

    pub fn collect(&self) -> Snapshot {
        let now = now_ms();
        let mut warnings = Vec::new();
        let mut sessions = claude::collect(&self.claude_roots, &mut warnings);

        if let Some(home) = &self.paseo_home {
            let permits = paseo::permits().unwrap_or_else(|| {
                warnings.push("paseo: permission requests unavailable".into());
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

        let mut snapshot = Snapshot {
            generated_at_ms: now,
            sessions,
            warnings,
        };
        snapshot.sort();
        snapshot
    }
}
