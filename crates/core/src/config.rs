use crate::error::Result;
use std::env;

#[derive(Debug, Clone)]
pub struct Config {
    pub host: String,
    pub port: u16,
    pub auth_token: Option<String>,
    pub searxng_url: Option<String>,
    pub searxng_token: Option<String>,
    pub cache_ttl_secs: u64,
    pub crawl_max_concurrency: usize,
    pub crawl_default_limit: usize,
    pub scrape_delay_preset: String,
    pub stealth_enabled: bool,
    pub cdp_enabled: bool,
    pub behavioral_simulation: bool,
    pub proxy_url: Option<String>,
    pub proxy_file: Option<String>,
    pub proxy_sticky_sessions: bool,
    pub proxy_cooldown_secs: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            host: "0.0.0.0".to_string(),
            port: 3002,
            auth_token: None,
            searxng_url: None,
            searxng_token: None,
            cache_ttl_secs: 172_800,
            crawl_max_concurrency: 5,
            crawl_default_limit: 10_000,
            scrape_delay_preset: "polite".to_string(),
            stealth_enabled: true,
            cdp_enabled: true,
            behavioral_simulation: true,
            proxy_url: None,
            proxy_file: None,
            proxy_sticky_sessions: true,
            proxy_cooldown_secs: 300,
        }
    }
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let mut cfg = Self::default();

        if let Ok(v) = env::var("HOST") {
            cfg.host = v;
        }
        if let Ok(v) = env::var("PORT") {
            cfg.port = v
                .parse()
                .map_err(|e| crate::error::CrwError::Config(format!("invalid PORT: {e}")))?;
        }
        if let Ok(v) = env::var("AUTH_TOKEN") {
            if !v.is_empty() {
                cfg.auth_token = Some(v);
            }
        }
        if let Ok(v) = env::var("SEARXNG_URL") {
            if !v.is_empty() {
                cfg.searxng_url = Some(v);
            }
        }
        if let Ok(v) = env::var("SEARXNG_TOKEN") {
            if !v.is_empty() {
                cfg.searxng_token = Some(v);
            }
        }
        if let Ok(v) = env::var("CACHE_TTL_SECS") {
            cfg.cache_ttl_secs = v.parse().map_err(|e| {
                crate::error::CrwError::Config(format!("invalid CACHE_TTL_SECS: {e}"))
            })?;
        }
        if let Ok(v) = env::var("CRAWL_MAX_CONCURRENCY") {
            cfg.crawl_max_concurrency = v.parse().map_err(|e| {
                crate::error::CrwError::Config(format!("invalid CRAWL_MAX_CONCURRENCY: {e}"))
            })?;
        }
        if let Ok(v) = env::var("CRAWL_DEFAULT_LIMIT") {
            cfg.crawl_default_limit = v.parse().map_err(|e| {
                crate::error::CrwError::Config(format!("invalid CRAWL_DEFAULT_LIMIT: {e}"))
            })?;
        }
        if let Ok(v) = env::var("SCRAPE_DELAY_PRESET") {
            cfg.scrape_delay_preset = v;
        }
        if let Ok(v) = env::var("STEALTH_ENABLED") {
            cfg.stealth_enabled = parse_bool(&v);
        }
        if let Ok(v) = env::var("CDP_ENABLED") {
            cfg.cdp_enabled = parse_bool(&v);
        }
        if let Ok(v) = env::var("BEHAVIORAL_SIMULATION") {
            cfg.behavioral_simulation = parse_bool(&v);
        }
        if let Ok(v) = env::var("PROXY_URL") {
            if !v.is_empty() {
                cfg.proxy_url = Some(v);
            }
        }
        if let Ok(v) = env::var("PROXY_FILE") {
            if !v.is_empty() {
                cfg.proxy_file = Some(v);
            }
        }
        if let Ok(v) = env::var("PROXY_STICKY_SESSIONS") {
            cfg.proxy_sticky_sessions = parse_bool(&v);
        }
        if let Ok(v) = env::var("PROXY_COOLDOWN_SECS") {
            cfg.proxy_cooldown_secs = v.parse().map_err(|e| {
                crate::error::CrwError::Config(format!("invalid PROXY_COOLDOWN_SECS: {e}"))
            })?;
        }

        Ok(cfg)
    }

    pub fn bind_addr(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }
}

fn parse_bool(v: &str) -> bool {
    matches!(
        v.to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on" | "y" | "t"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // Env-var tests must not run concurrently because they mutate process-wide
    // environment variables.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn default_config_has_expected_values() {
        let cfg = Config::default();
        assert_eq!(cfg.host, "0.0.0.0");
        assert_eq!(cfg.port, 3002);
        assert!(cfg.auth_token.is_none());
        assert!(cfg.stealth_enabled);
        assert_eq!(cfg.scrape_delay_preset, "polite");
        assert_eq!(cfg.cache_ttl_secs, 172_800);
    }

    #[test]
    fn from_env_overrides() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("PORT", "4321");
        std::env::set_var("AUTH_TOKEN", "secret");
        std::env::set_var("STEALTH_ENABLED", "false");
        std::env::set_var("SEARXNG_URL", "http://example.com/");
        std::env::set_var("CRAWL_MAX_CONCURRENCY", "10");

        let cfg = Config::from_env().unwrap();
        assert_eq!(cfg.port, 4321);
        assert_eq!(cfg.auth_token.as_deref(), Some("secret"));
        assert!(!cfg.stealth_enabled);
        assert_eq!(cfg.searxng_url.as_deref(), Some("http://example.com/"));
        assert_eq!(cfg.crawl_max_concurrency, 10);

        std::env::remove_var("PORT");
        std::env::remove_var("AUTH_TOKEN");
        std::env::remove_var("STEALTH_ENABLED");
        std::env::remove_var("SEARXNG_URL");
        std::env::remove_var("CRAWL_MAX_CONCURRENCY");
    }

    #[test]
    fn from_env_invalid_port() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("PORT", "not-a-number");
        let err = Config::from_env().unwrap_err();
        match err {
            crate::error::CrwError::Config(_) => {}
            _ => panic!("expected Config error"),
        }
        std::env::remove_var("PORT");
    }

    #[test]
    fn bind_addr_format() {
        let cfg = Config::default();
        assert_eq!(cfg.bind_addr(), "0.0.0.0:3002");
    }
}
