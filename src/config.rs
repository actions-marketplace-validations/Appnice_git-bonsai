//! Growth settings: defaults, overridden by `bonsai/bonsai.toml`, overridden by CLI flags.

use serde::Deserialize;
use std::collections::BTreeMap;

/// Configuration errors (exit code 2 in the CLI).
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ConfigError {
    #[error("max_branches must be 1..=20, got {0}")]
    MaxBranches(usize),
    #[error("max_growth_per_day must be 1..=8, got {0}")]
    MaxGrowth(u32),
    #[error("{0} must be at least 1")]
    ZeroDays(&'static str),
    #[error("colour for {lang} must be #rrggbb, got {value:?}")]
    Color { lang: String, value: String },
}

/// What `bonsai.toml` may set. Every field is optional in the file.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Living branches at most (top repos by commits in the last 365 days).
    pub max_branches: usize,
    /// Repo name globs (`*`, `?`, case-insensitive) that never get a branch.
    pub exclude: Vec<String>,
    /// Repo names that always get a branch, even outside the top `max_branches`.
    pub pin: Vec<String>,
    /// Cells a branch may gain per day (anti-farming).
    pub max_growth_per_day: u32,
    pub bloom_streak_days: u32,
    pub dormant_after_days: u32,
    /// Language colour overrides, `{"Kotlin": "#a97bff"}`.
    pub colors: BTreeMap<String, String>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            max_branches: 16,
            exclude: Vec::new(),
            pin: Vec::new(),
            max_growth_per_day: 1,
            bloom_streak_days: 21,
            dormant_after_days: 60,
            colors: BTreeMap::new(),
        }
    }
}

impl Config {
    /// Checks ranges; call after merging file and flags.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if !(1..=crate::rules::MAX_BRANCHES).contains(&self.max_branches) {
            return Err(ConfigError::MaxBranches(self.max_branches));
        }
        if !(1..=8).contains(&self.max_growth_per_day) {
            return Err(ConfigError::MaxGrowth(self.max_growth_per_day));
        }
        if self.bloom_streak_days == 0 {
            return Err(ConfigError::ZeroDays("bloom_streak_days"));
        }
        if self.dormant_after_days == 0 {
            return Err(ConfigError::ZeroDays("dormant_after_days"));
        }
        for (lang, value) in &self.colors {
            let ok = value.len() == 7
                && value.starts_with('#')
                && value[1..].chars().all(|c| c.is_ascii_hexdigit());
            if !ok {
                return Err(ConfigError::Color {
                    lang: lang.clone(),
                    value: value.clone(),
                });
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_valid() {
        assert_eq!(Config::default().validate(), Ok(()));
    }

    #[test]
    fn rejects_bad_values() {
        let c = Config {
            max_branches: 0,
            ..Config::default()
        };
        assert_eq!(c.validate(), Err(ConfigError::MaxBranches(0)));
        let mut c = Config::default();
        c.colors.insert("Kotlin".into(), "a97bff".into());
        assert!(matches!(c.validate(), Err(ConfigError::Color { .. })));
    }

    #[test]
    fn partial_json_fills_defaults() {
        let c: Config = serde_json::from_str(r#"{"pin":["ferrite"]}"#).unwrap();
        assert_eq!(c.pin, vec!["ferrite".to_string()]);
        assert_eq!(c.max_branches, 16);
    }
}
