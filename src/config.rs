use serde::Deserialize;
use std::path::PathBuf;

/// `~/.config/oncue/config.toml`. Every field is optional.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Config {
    /// `en` or `ko`. Empty means detect from `LANG`.
    pub language: String,
    /// Extra Claude Code config roots beyond the auto-discovered ones.
    pub claude_config_dirs: Vec<PathBuf>,
    /// Read Paseo agents and permission requests.
    pub paseo: bool,
    /// Finished turns older than this move to the dormant list.
    pub dormant_after_minutes: u64,
    /// Refresh interval in seconds.
    pub interval_secs: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            language: String::new(),
            claude_config_dirs: vec![],
            paseo: true,
            dormant_after_minutes: 6 * 60,
            interval_secs: 2,
        }
    }
}

impl Config {
    pub fn path() -> Option<PathBuf> {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| dirs::home_dir().map(|h| h.join(".config")))?;
        Some(base.join("oncue/config.toml"))
    }

    pub fn load() -> anyhow::Result<Self> {
        let Some(path) = Self::path().filter(|p| p.is_file()) else {
            return Ok(Self::default());
        };
        let text = std::fs::read_to_string(&path)?;
        let mut config: Self =
            toml::from_str(&text).map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?;
        config.claude_config_dirs = config
            .claude_config_dirs
            .into_iter()
            .map(expand_home)
            .collect();
        Ok(config)
    }
}

fn expand_home(p: PathBuf) -> PathBuf {
    match (p.strip_prefix("~"), dirs::home_dir()) {
        (Ok(rest), Some(home)) => home.join(rest),
        _ => p,
    }
}
