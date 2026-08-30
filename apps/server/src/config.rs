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
    /// MongoDB connection string. The server requires a replica set for
    /// multi-document transactions (single-node replica set in development).
    pub mongodb_uri: String,
    /// MongoDB database name holding the Janus collections.
    pub mongodb_database: String,
    /// Directory holding the built web client. When set, the HTTP transport
    /// serves it from the same origin as `/api` and `/health`, which is how the
    /// deployment image ships frontend and backend as one process. Unset in
    /// development, where Vite serves the client and proxies the API.
    pub web_dist: Option<PathBuf>,
    pub mode: RunMode,
    pub development_auth: bool,
    /// Authentication scheme the deployment uses. `Passkey` is the default and
    /// requires a real https domain; `Totp` is for http/IP deployments where
    /// WebAuthn cannot run and the owner signs in with a time-based one-time
    /// password instead.
    pub auth_mode: janus_identity::AuthMode,
    pub webauthn_rp_name: String,
    pub webauthn_rp_id: String,
    pub public_origin: url::Url,
    pub event_heartbeat: Duration,
    pub automation_webhook_enabled: bool,
    pub automation_webhook_secret: Option<String>,
    pub automation_github_token: Option<String>,
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("JANUS_BIND must be a valid socket address: {0}")]
    InvalidBind(String),
    #[error("JANUS_MODE must be development or production")]
    InvalidMode,
    #[error("JANUS_DEV_AUTH must be true or false")]
    InvalidDevelopmentAuth,
    #[error("JANUS_AUTH_MODE must be passkey or totp")]
    InvalidAuthMode,
    #[error("development authentication is forbidden in production")]
    UnsafeProductionAuth,
    #[error("development authentication requires a loopback bind address")]
    UnsafeDevelopmentBind,
    #[error("JANUS_PUBLIC_ORIGIN must be an absolute http(s) origin without a path: {0}")]
    InvalidPublicOrigin(String),
    #[error("production WebAuthn requires an https public origin")]
    InsecureProductionOrigin,
    #[error("JANUS_MONGODB_URI must be a mongodb:// or mongodb+srv:// connection string: {0}")]
    InvalidMongodbUri(String),
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
        let auth_mode = match env::var("JANUS_AUTH_MODE")
            .unwrap_or_else(|_| "passkey".into())
            .as_str()
        {
            "passkey" => janus_identity::AuthMode::Passkey,
            "totp" => janus_identity::AuthMode::Totp,
            _ => return Err(ConfigError::InvalidAuthMode),
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
            mongodb_uri: env::var("JANUS_MONGODB_URI")
                .unwrap_or_else(|_| "mongodb://localhost:27017/?replicaSet=rs0".into()),
            mongodb_database: env::var("JANUS_MONGODB_DATABASE").unwrap_or_else(|_| "janus".into()),
            web_dist: env::var_os("JANUS_WEB_DIST")
                .map(PathBuf::from)
                .filter(|path| !path.as_os_str().is_empty()),
            mode,
            development_auth,
            auth_mode,
            webauthn_rp_name: env::var("JANUS_WEBAUTHN_RP_NAME").unwrap_or_else(|_| "Janus".into()),
            webauthn_rp_id: env::var("JANUS_WEBAUTHN_RP_ID")
                .unwrap_or_else(|_| public_origin.host_str().unwrap_or("localhost").into()),
            public_origin,
            event_heartbeat: Duration::from_secs(15),
            automation_webhook_enabled: parse_bool_env("JANUS_AUTOMATION_WEBHOOK_ENABLED", false),
            automation_webhook_secret: env::var("JANUS_AUTOMATION_WEBHOOK_SECRET")
                .ok()
                .filter(|value| !value.trim().is_empty()),
            automation_github_token: env::var("JANUS_AUTOMATION_GITHUB_TOKEN")
                .ok()
                .filter(|value| !value.trim().is_empty()),
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
        // WebAuthn only works on a secure context, so passkey deployments must
        // serve https in production. TOTP mode has no such requirement and is
        // the supported way to run an http/IP deployment.
        if self.mode == RunMode::Production
            && self.auth_mode == janus_identity::AuthMode::Passkey
            && self.public_origin.scheme() != "https"
        {
            return Err(ConfigError::InsecureProductionOrigin);
        }
        if !self.mongodb_uri.starts_with("mongodb://")
            && !self.mongodb_uri.starts_with("mongodb+srv://")
        {
            return Err(ConfigError::InvalidMongodbUri(self.mongodb_uri.clone()));
        }
        Ok(())
    }
}

fn parse_bool_env(name: &str, default: bool) -> bool {
    match env::var(name).ok().as_deref() {
        Some("1") | Some("true") | Some("TRUE") => true,
        Some("0") | Some("false") | Some("FALSE") => false,
        _ => default,
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
            mongodb_uri: "mongodb://localhost:27017/?replicaSet=rs0".into(),
            mongodb_database: "janus_test".into(),
            web_dist: None,
            mode: RunMode::Production,
            development_auth: true,
            auth_mode: janus_identity::AuthMode::Passkey,
            webauthn_rp_name: "Janus".into(),
            webauthn_rp_id: "localhost".into(),
            public_origin: url::Url::parse("https://localhost").expect("static URL"),
            event_heartbeat: Duration::from_secs(15),
            automation_webhook_enabled: false,
            automation_webhook_secret: None,
            automation_github_token: None,
        };

        assert!(matches!(
            config.validate(),
            Err(ConfigError::UnsafeProductionAuth)
        ));
    }
}
