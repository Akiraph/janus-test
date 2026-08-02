use std::{env, net::IpAddr, net::SocketAddr, path::PathBuf, str::FromStr, time::Duration};

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
    pub webauthn_rp_id: String,
    pub public_origin: url::Url,
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
    #[error("development authentication requires a loopback bind address")]
    UnsafeDevelopmentBind,
    #[error("JANUS_PUBLIC_ORIGIN must be an absolute http(s) origin without a path: {0}")]
    InvalidPublicOrigin(String),
    #[error("production WebAuthn requires an https public origin")]
    InsecureProductionOrigin,
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
        let origin_value =
            env::var("JANUS_PUBLIC_ORIGIN").unwrap_or_else(|_| format!("http://{}", bind));
        let public_origin = url::Url::parse(&origin_value)
            .map_err(|_| ConfigError::InvalidPublicOrigin(origin_value.clone()))?;
        // Always absolute: relative data_root (the ".janus-dev" default) is resolved
        // against process cwd, and later `git worktree add` runs with
        // `current_dir = main_repo`. A relative session path would then be
        // created *inside* the project Main clone — leaking `.janus-dev`
        // into the workspace tree. Canonicalize up front so every consumer
        // (clone, worktree, manifest) sees the same absolute root.
        let data_root = env::var_os("JANUS_DATA_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(".janus-dev"));
        let data_root = if data_root.is_absolute() {
            data_root
        } else {
            std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(data_root)
        };
        let config = Self {
            bind,
            data_root,
            mode,
            development_auth,
            webauthn_rp_name: env::var("JANUS_WEBAUTHN_RP_NAME").unwrap_or_else(|_| "Janus".into()),
            webauthn_rp_id: env::var("JANUS_WEBAUTHN_RP_ID")
                .unwrap_or_else(|_| public_origin.host_str().unwrap_or("localhost").into()),
            public_origin,
            event_heartbeat: Duration::from_secs(15),
        };
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.mode == RunMode::Production && self.development_auth {
            return Err(ConfigError::UnsafeProductionAuth);
        }
        if self.development_auth
            && !matches!(self.bind.ip(), IpAddr::V4(ip) if ip.is_loopback())
            && !matches!(self.bind.ip(), IpAddr::V6(ip) if ip.is_loopback())
        {
            return Err(ConfigError::UnsafeDevelopmentBind);
        }
        if !matches!(self.public_origin.scheme(), "http" | "https")
            || self.public_origin.cannot_be_a_base()
            || self.public_origin.path() != "/"
            || self.public_origin.query().is_some()
            || self.public_origin.fragment().is_some()
        {
            return Err(ConfigError::InvalidPublicOrigin(
                self.public_origin.to_string(),
            ));
        }
        if self.mode == RunMode::Production && self.public_origin.scheme() != "https" {
            return Err(ConfigError::InsecureProductionOrigin);
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
            webauthn_rp_id: "localhost".into(),
            public_origin: url::Url::parse("https://localhost").expect("static URL"),
            event_heartbeat: Duration::from_secs(15),
        };

        assert!(matches!(
            config.validate(),
            Err(ConfigError::UnsafeProductionAuth)
        ));
    }
}
