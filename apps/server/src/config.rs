use std::{env, net::SocketAddr, path::PathBuf, str::FromStr, time::Duration};

use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunMode {
    Development,
    Production,
}

impl RunMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Development => "development",
            Self::Production => "production",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    pub bind: SocketAddr,
    pub data_root: PathBuf,
    pub mode: RunMode,
    pub development_auth: bool,
    pub webauthn_rp_name: String,
    pub event_heartbeat: Duration,
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("JANUS_BIND must be a valid socket address: {0}")]
    InvalidBind(String),
    #[error("JANUS_MODE must be development or production")]
    InvalidMode,
    #[error("JANUS_DEV_AUTH must be true or false")]
    InvalidDevelopmentAuth,
    #[error("development authentication is forbidden in production")]
    UnsafeProductionAuth,
}

impl Config {
    pub fn from_env() -> Result<Self, ConfigError> {
        let bind_value = env::var("JANUS_BIND").unwrap_or_else(|_| "127.0.0.1:4317".into());
        let bind = SocketAddr::from_str(&bind_value)
            .map_err(|_| ConfigError::InvalidBind(bind_value.clone()))?;
        let mode = match env::var("JANUS_MODE")
            .unwrap_or_else(|_| "development".into())
            .as_str()
        {
            "development" => RunMode::Development,
            "production" => RunMode::Production,
            _ => return Err(ConfigError::InvalidMode),
        };
        let development_auth = match env::var("JANUS_DEV_AUTH")
            .unwrap_or_else(|_| (mode == RunMode::Development).to_string())
            .as_str()
        {
            "true" | "1" => true,
            "false" | "0" => false,
            _ => return Err(ConfigError::InvalidDevelopmentAuth),
        };
        let config = Self {
            bind,
            data_root: env::var_os("JANUS_DATA_ROOT")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(".janus-dev")),
            mode,
            development_auth,
            webauthn_rp_name: env::var("JANUS_WEBAUTHN_RP_NAME").unwrap_or_else(|_| "Janus".into()),
            event_heartbeat: Duration::from_secs(15),
        };
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.mode == RunMode::Production && self.development_auth {
            return Err(ConfigError::UnsafeProductionAuth);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{Config, ConfigError, RunMode};
    use std::{net::SocketAddr, path::PathBuf, time::Duration};

    #[test]
    fn production_rejects_development_auth() {
        let config = Config {
            bind: SocketAddr::from(([127, 0, 0, 1], 0)),
            data_root: PathBuf::from("unused"),
            mode: RunMode::Production,
            development_auth: true,
            webauthn_rp_name: "Janus".into(),
            event_heartbeat: Duration::from_secs(15),
        };

        assert!(matches!(
            config.validate(),
            Err(ConfigError::UnsafeProductionAuth)
        ));
    }
}
