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
    pub github: GitHubConfig,
    pub linear: LinearConfig,
    /// Command printing per-account usage limits as JSON (see README).
    pub usage_command: String,
    pub usage_interval_secs: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct LinearConfig {
    pub enabled: bool,
    /// Environment variable holding a Linear personal API key.
    pub api_key_env: String,
    /// Shell command that prints the key, used when the variable is unset,
    /// e.g. `security find-generic-password -s oncue-linear -w`.
    pub api_key_command: String,
    /// Issue key prefixes to match. Empty means the workspace's teams, or any
    /// `ABC-123` without an API key.
    pub team_keys: Vec<String>,
    pub interval_secs: u64,
}

impl Default for LinearConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            api_key_env: "LINEAR_API_KEY".into(),
            api_key_command: String::new(),
            team_keys: vec![],
            interval_secs: 120,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct GitHubConfig {
    /// Look up PR state with the `gh` CLI.
    pub enabled: bool,
    /// Also list your open PRs that no live session owns.
    pub my_prs: bool,
    pub interval_secs: u64,
    /// PRs untouched for longer than this stop raising attention.
    pub stale_after_days: u64,
    /// Limit your own PRs to these users or orgs. Empty means all.
    pub owners: Vec<String>,
}

impl Default for GitHubConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            my_prs: true,
            interval_secs: 90,
            stale_after_days: 3,
            owners: vec![],
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            language: String::new(),
            claude_config_dirs: vec![],
            paseo: true,
            dormant_after_minutes: 6 * 60,
            interval_secs: 2,
            github: GitHubConfig::default(),
            linear: LinearConfig::default(),
            usage_command: String::new(),
            usage_interval_secs: 120,
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
