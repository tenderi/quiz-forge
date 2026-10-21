//! Configuration from the environment (DESIGN.md §11).

use crate::domain::voting::VoteConfig;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct AgentConfig {
    pub enabled: bool,
    pub api_key: Option<String>,
    pub model: String,
    pub api_url: String,
    pub max_questions_per_run: u32,
    pub max_cost_usd_per_run: f64,
    pub interval_hours: u32,
    pub send_media: bool,
}

impl Default for AgentConfig {
    fn default() -> Self {
        AgentConfig {
            enabled: false,
            api_key: None,
            model: "claude-sonnet-5-5".into(),
            api_url: "https://api.anthropic.com".into(),
            max_questions_per_run: 25,
            max_cost_usd_per_run: 1.0,
            interval_hours: 24,
            send_media: false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    pub telegram_token: Option<String>,
    pub group_chat_id: i64,
    pub public_base_url: String,
    pub listen: String,
    pub data_dir: PathBuf,
    pub default_language: String,
    pub vote: VoteConfig,
    pub media_max_image_bytes: u64,
    pub media_max_audio_bytes: u64,
    pub agent: AgentConfig,
}

impl Config {
    /// Defaults suitable for tests: no Telegram, in-place data dir.
    pub fn for_tests(data_dir: PathBuf) -> Config {
        Config {
            telegram_token: None,
            group_chat_id: -100,
            public_base_url: "http://localhost:8090".into(),
            listen: "127.0.0.1:0".into(),
            data_dir,
            default_language: "fi".into(),
            vote: VoteConfig::default(),
            media_max_image_bytes: 5 * 1024 * 1024,
            media_max_audio_bytes: 20 * 1024 * 1024,
            agent: AgentConfig::default(),
        }
    }

    pub fn from_env() -> anyhow::Result<Config> {
        Self::from_lookup(|k| std::env::var(k).ok())
    }

    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> anyhow::Result<Config> {
        use anyhow::Context;
        let get = |k: &str| get(k).filter(|v| !v.trim().is_empty());
        let num = |k: &str, d: u64| -> anyhow::Result<u64> {
            get(k)
                .map(|v| {
                    v.trim()
                        .parse::<u64>()
                        .with_context(|| format!("{k} must be a number"))
                })
                .unwrap_or(Ok(d))
        };
        let group_chat_id = get("TELEGRAM_GROUP_ID")
            .context("TELEGRAM_GROUP_ID is required (the group's chat id, e.g. -1001234567890)")?
            .trim()
            .parse::<i64>()
            .context("TELEGRAM_GROUP_ID must be a number")?;
        let public_base_url = get("PUBLIC_BASE_URL")
            .unwrap_or_else(|| "http://localhost:8090".into())
            .trim_end_matches('/')
            .to_string();
        url::Url::parse(&public_base_url).context("PUBLIC_BASE_URL must be a URL")?;
        let min_approvals = num("VOTE_MIN_APPROVALS", 1)? as u32;
        let window_hours = num("VOTE_WINDOW_HOURS", 24 * 7)?;
        if window_hours == 0 {
            anyhow::bail!("VOTE_WINDOW_HOURS must be at least 1");
        }
        let agent_enabled = matches!(get("AGENT_ENABLED").as_deref(), Some("true" | "1" | "yes"));
        let defaults = AgentConfig::default();
        let agent = AgentConfig {
            enabled: agent_enabled,
            api_key: get("ANTHROPIC_API_KEY"),
            model: get("AGENT_MODEL").unwrap_or(defaults.model),
            api_url: get("AGENT_API_URL").unwrap_or(defaults.api_url),
            max_questions_per_run: num("AGENT_MAX_QUESTIONS_PER_RUN", 25)? as u32,
            max_cost_usd_per_run: get("AGENT_MAX_COST_USD_PER_RUN")
                .map(|v| {
                    v.parse::<f64>()
                        .context("AGENT_MAX_COST_USD_PER_RUN must be a number")
                })
                .unwrap_or(Ok(1.0))?,
            interval_hours: num("AGENT_INTERVAL_HOURS", 24)?.max(1) as u32,
            send_media: matches!(
                get("AGENT_SEND_MEDIA").as_deref(),
                Some("true" | "1" | "yes")
            ),
        };
        if agent.enabled && agent.api_key.is_none() {
            anyhow::bail!("AGENT_ENABLED is set but ANTHROPIC_API_KEY is missing");
        }
        Ok(Config {
            telegram_token: get("TELEGRAM_BOT_TOKEN"),
            group_chat_id,
            public_base_url,
            listen: get("LISTEN").unwrap_or_else(|| "127.0.0.1:8090".into()),
            data_dir: PathBuf::from(get("DATA_DIR").unwrap_or_else(|| "./var".into())),
            default_language: get("DEFAULT_LANGUAGE")
                .unwrap_or_else(|| "fi".into())
                .to_lowercase(),
            vote: VoteConfig {
                window: chrono::Duration::hours(window_hours as i64),
                min_approvals,
            },
            media_max_image_bytes: num("MEDIA_MAX_IMAGE_MB", 5)? * 1024 * 1024,
            media_max_audio_bytes: num("MEDIA_MAX_AUDIO_MB", 20)? * 1024 * 1024,
            agent,
        })
    }

    pub fn https(&self) -> bool {
        self.public_base_url.starts_with("https://")
    }

    pub fn db_path(&self) -> PathBuf {
        self.data_dir.join("quiz.db")
    }

    pub fn media_dir(&self) -> PathBuf {
        self.data_dir.join("media")
    }

    pub fn url(&self, path: &str) -> String {
        format!("{}{}", self.public_base_url, path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn cfg(pairs: &[(&str, &str)]) -> anyhow::Result<Config> {
        let m: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        Config::from_lookup(|k| m.get(k).cloned())
    }

    #[test]
    fn defaults_and_required() {
        assert!(cfg(&[]).is_err());
        let c = cfg(&[
            ("TELEGRAM_GROUP_ID", "-1001"),
            ("PUBLIC_BASE_URL", "https://q.example/"),
        ])
        .unwrap();
        assert_eq!(c.group_chat_id, -1001);
        assert_eq!(c.public_base_url, "https://q.example");
        assert!(c.https());
        assert_eq!(c.url("/x"), "https://q.example/x");
        assert_eq!(c.vote.window, chrono::Duration::days(7));
        assert_eq!(c.vote.min_approvals, 1);
        assert_eq!(c.listen, "127.0.0.1:8090");
        assert_eq!(c.db_path(), PathBuf::from("./var/quiz.db"));
        assert_eq!(c.media_dir(), PathBuf::from("./var/media"));
        assert!(!c.agent.enabled);
        assert_eq!(c.media_max_image_bytes, 5 * 1024 * 1024);
    }

    #[test]
    fn overrides_and_errors() {
        let c = cfg(&[
            ("TELEGRAM_GROUP_ID", "-5"),
            ("VOTE_WINDOW_HOURS", "48"),
            ("VOTE_MIN_APPROVALS", "2"),
            ("DEFAULT_LANGUAGE", "EN"),
            ("AGENT_ENABLED", "true"),
            ("ANTHROPIC_API_KEY", "k"),
            ("AGENT_MAX_COST_USD_PER_RUN", "0.5"),
            ("AGENT_SEND_MEDIA", "1"),
        ])
        .unwrap();
        assert_eq!(c.vote.window, chrono::Duration::hours(48));
        assert_eq!(c.vote.min_approvals, 2);
        assert_eq!(c.default_language, "en");
        assert!(c.agent.enabled && c.agent.send_media);
        assert_eq!(c.agent.max_cost_usd_per_run, 0.5);
        assert!(!c.https());

        assert!(cfg(&[("TELEGRAM_GROUP_ID", "x")]).is_err());
        assert!(cfg(&[("TELEGRAM_GROUP_ID", "1"), ("VOTE_WINDOW_HOURS", "0")]).is_err());
        assert!(cfg(&[("TELEGRAM_GROUP_ID", "1"), ("VOTE_WINDOW_HOURS", "a")]).is_err());
        assert!(cfg(&[("TELEGRAM_GROUP_ID", "1"), ("AGENT_ENABLED", "true")]).is_err());
        assert!(cfg(&[("TELEGRAM_GROUP_ID", "1"), ("PUBLIC_BASE_URL", "not a url")]).is_err());
        assert!(
            cfg(&[
                ("TELEGRAM_GROUP_ID", "1"),
                ("AGENT_MAX_COST_USD_PER_RUN", "lots")
            ])
            .is_err()
        );
    }
}
