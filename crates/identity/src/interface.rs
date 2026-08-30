//! Owner authentication, passkey ceremonies, and TOTP login.

use std::{
    sync::{Arc, Mutex},
    time::Instant,
};

use anyhow::Context;
use chrono::{DateTime, Duration, Utc};
use futures_util::TryStreamExt;
use janus_infrastructure::clock::{format_utc, now_utc_str};
use mongodb::{
    ClientSession, Database,
    bson::{Bson, Document, doc},
};
use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha1::{Digest, Sha1};
use thiserror::Error;
use utoipa::ToSchema;
use uuid::Uuid;
use webauthn_rs::{
    Webauthn, WebauthnBuilder,
    prelude::{
        Passkey, PasskeyAuthentication, PasskeyRegistration, PublicKeyCredential,
        RegisterPublicKeyCredential,
    },
};

use janus_infrastructure::{
    id::{OwnerId, PasskeyId},
    secrets::{purpose_hash, random_bytes, random_token},
};

const CEREMONY_TTL: Duration = Duration::minutes(5);
const SESSION_IDLE_TTL: Duration = Duration::days(7);
const RECOVERY_TTL: Duration = Duration::minutes(10);
const TOTP_INIT_TTL: Duration = Duration::minutes(15);
const TOTP_WINDOW_SECS: i64 = 30;
const RATE_WINDOW_SECS: u64 = 60;
const RATE_MAX_ATTEMPTS: usize = 5;

/// Which primary authentication mechanism the deployment uses. Passkey is the
/// default and requires an https domain origin; TOTP is meant for deployments
/// served over plain http or an IP address, where WebAuthn cannot work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AuthMode {
    Passkey,
    Totp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InitializationState {
    Uninitialized,
    Initialized,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct CeremonyOptions {
    pub ceremony_id: String,
    pub public_key: Value,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct OwnerView {
    pub id: String,
    pub display_name: String,
    pub authentication_mode: AuthenticationMode,
    pub csrf_token: String,
}

#[derive(Debug, Clone, Copy, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AuthenticationMode {
    Passkey,
    Totp,
    Development,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct TotpProvision {
    pub ceremony_id: String,
    /// Base32 (RFC 4648) TOTP shared secret, for manual entry into an
    /// authenticator app.
    pub secret_base32: String,
    /// `otpauth://` URI carrying the same secret, scannable as a QR code.
    pub otpauth_uri: String,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PasskeyView {
    pub id: String,
    pub name: String,
    pub created_at: String,
    pub last_used_at: Option<String>,
}

#[derive(Debug)]
pub struct AuthenticationGrant {
    pub owner: OwnerView,
    pub session_token: String,
    pub recovery_codes: Option<Vec<String>>,
}

#[derive(Debug)]
pub struct RecoveryGrant {
    pub token: String,
    pub expires_at: String,
}

#[derive(Debug, Clone)]
pub struct AuthContext {
    pub owner_id: String,
    pub display_name: String,
    pub session_id: Option<String>,
    pub csrf_token: String,
    pub development: bool,
}

#[derive(Debug, Error)]
pub enum IdentityError {
    #[error("authentication is required")]
    AuthRequired,
    #[error("initialization has already completed")]
    AlreadyInitialized,
    #[error("the initialization token is invalid or expired")]
    InvalidInitializationToken,
    #[error("the ceremony is invalid or expired")]
    InvalidCeremony,
    #[error("the credential could not be verified")]
    InvalidCredential,
    #[error("the recovery code is invalid or expired")]
    InvalidRecoveryCode,
    #[error("the final passkey cannot be removed")]
    LastPasskey,
    #[error("the requested passkey does not exist")]
    PasskeyNotFound,
    #[error("the request origin is not allowed")]
    OriginRejected,
    #[error("the CSRF token is invalid")]
    CsrfRejected,
    #[error("passkey authentication is disabled")]
    PasskeyDisabled,
    #[error("totp authentication is disabled")]
    TotpDisabled,
    #[error("the TOTP code is invalid")]
    InvalidTotpCode,
    #[error("too many failed attempts; try again later")]
    RateLimited,
    #[error("identity storage failed")]
    Storage(#[from] mongodb::error::Error),
    #[error("identity data is invalid")]
    Data(#[from] serde_json::Error),
    #[error("identity operation failed")]
    Internal(#[from] anyhow::Error),
    #[error("document value access error: {0}")]
    ValueAccess(#[from] mongodb::bson::document::ValueAccessError),
}

#[derive(Clone)]
pub struct IdentityInterface {
    pool: Database,
    /// `None` when `auth_mode` is TOTP, so the deployment does not need a
    /// WebAuthn-compatible origin.
    webauthn: Option<Webauthn>,
    origin: String,
    development_auth: bool,
    auth_mode: AuthMode,
    /// Shared sliding-window limiter over failed TOTP code attempts. Janus has
    /// a single owner, so a deployment-wide budget is the right granularity.
    rate: Arc<Mutex<LoginRate>>,
}

/// In-memory sliding window of recent failed TOTP attempts.
#[derive(Default)]
struct LoginRate {
    failures: std::collections::VecDeque<Instant>,
}

impl LoginRate {
    /// Whether a new attempt is allowed within the current window.
    fn allow(&mut self) -> bool {
        let now = Instant::now();
        while self
            .failures
            .front()
            .is_some_and(|at| now.duration_since(*at).as_secs() > RATE_WINDOW_SECS)
        {
            self.failures.pop_front();
        }
        self.failures.len() < RATE_MAX_ATTEMPTS
    }

    fn record(&mut self) {
        self.failures.push_back(Instant::now());
    }
}

#[derive(Serialize, Deserialize)]
struct RegistrationState {
    token_hash: Option<String>,
    owner_id: String,
    display_name: String,
    passkey_name: String,
    recovery_state_id: Option<String>,
    state: PasskeyRegistration,
}

#[derive(Serialize, Deserialize)]
struct AuthenticationState {
    state: PasskeyAuthentication,
}

/// Pending TOTP enrollment during initialization. Stored as a ceremony so the
/// secret is held server-side until the owner proves they scanned it.
#[derive(Serialize, Deserialize)]
struct TotpInitializeState {
    token_hash: String,
    owner_id: String,
    display_name: String,
    secret_base32: String,
}

struct OwnerRow {
    id: String,
    display_name: String,
}

impl OwnerRow {
    fn from_doc(doc: &Document) -> Result<Self, IdentityError> {
        Ok(Self {
            id: doc.get_str("_id")?.to_owned(),
            display_name: doc.get_str("display_name")?.to_owned(),
        })
    }
}

impl IdentityInterface {
    pub fn new(
        pool: Database,
        webauthn_rp_id: &str,
        public_origin: &url::Url,
        webauthn_rp_name: &str,
        development_auth: bool,
        auth_mode: AuthMode,
    ) -> anyhow::Result<Self> {
        let webauthn = if auth_mode == AuthMode::Passkey {
            // webauthn-rs reports every builder rejection as a bare "The
            // configuration was invalid", so name both inputs and the constraint
            // that links them: the RP ID must be the origin host or a registrable
            // suffix of it, and an IP-address host has no domain to match at all.
            Some(
                WebauthnBuilder::new(webauthn_rp_id, public_origin)
                    .and_then(|builder| builder.rp_name(webauthn_rp_name).build())
                    .with_context(|| {
                        format!(
                            "build WebAuthn from JANUS_WEBAUTHN_RP_ID={webauthn_rp_id:?} and \
                             JANUS_PUBLIC_ORIGIN={public_origin} (origin host {:?} must be a domain \
                             name, not an IP address, and must equal or end with the RP ID)",
                            public_origin.host_str().unwrap_or_default()
                        )
                    })?,
            )
        } else {
            None
        };
        let service = Self {
            pool,
            webauthn,
            origin: public_origin.origin().ascii_serialization(),
            development_auth,
            auth_mode,
            rate: Arc::new(Mutex::new(LoginRate::default())),
        };
        Ok(service)
    }

    /// Access the WebAuthn engine, rejecting the call when the deployment uses
    /// TOTP authentication instead of passkeys.
    fn webauthn(&self) -> Result<&Webauthn, IdentityError> {
        self.webauthn.as_ref().ok_or(IdentityError::PasskeyDisabled)
    }

    pub async fn initialization_state(&self) -> Result<InitializationState, IdentityError> {
        let count = self
            .pool
            .collection::<Document>("owners")
            .count_documents(doc! {})
            .await?;
        Ok(if count == 0 {
            InitializationState::Uninitialized
        } else {
            InitializationState::Initialized
        })
    }

    pub async fn issue_initialization_token(&self) -> Result<String, IdentityError> {
        if self.initialization_state().await? == InitializationState::Initialized {
            return Err(IdentityError::AlreadyInitialized);
        }
        let token = random_token(32);
        let now = Utc::now();
        let mut session = self.pool.client().start_session().await?;
        session.start_transaction().await?;
        self.pool
            .collection::<Document>("initialization_tokens")
            .delete_many(doc! {"used_at": null})
            .session(&mut session)
            .await?;
        let id = Uuid::now_v7().to_string();
        let token_hash = purpose_hash("initialization-token", &token);
        let expires_at = format_utc(now + Duration::minutes(30));
        let created_at = format_utc(now);
        self.pool
            .collection::<Document>("initialization_tokens")
            .insert_one(doc! {
                "_id": &id,
                "token_hash": &token_hash,
                "expires_at": &expires_at,
                "created_at": &created_at,
            })
            .session(&mut session)
            .await?;
        session.commit_transaction().await?;
        Ok(token)
    }

    pub async fn issue_recovery_token(&self) -> Result<String, IdentityError> {
        let owner = self.owner().await?;
        let token = random_token(32);
        let now = Utc::now();
        let id = Uuid::now_v7().to_string();
        let token_hash = purpose_hash("recovery-state", &token);
        let expires_at = format_utc(now + RECOVERY_TTL);
        self.pool
            .collection::<Document>("recovery_states")
            .insert_one(doc! {
                "_id": &id,
                "owner_id": &owner.id,
                "token_hash": &token_hash,
                "expires_at": &expires_at,
            })
            .await?;
        Ok(token)
    }

    pub async fn initialize_options(
        &self,
        token: &str,
        display_name: &str,
    ) -> Result<CeremonyOptions, IdentityError> {
        if self.initialization_state().await? == InitializationState::Initialized {
            return Err(IdentityError::AlreadyInitialized);
        }
        let token_hash = purpose_hash("initialization-token", token);
        let now = now_utc_str();
        let valid = self
            .pool
            .collection::<Document>("initialization_tokens")
            .count_documents(doc! {
                "token_hash": &token_hash,
                "used_at": null,
                "expires_at": {"$gt": &now},
            })
            .await?;
        if valid != 1 {
            return Err(IdentityError::InvalidInitializationToken);
        }
        let owner_id = OwnerId::new().to_string();
        self.registration_options(RegistrationStateSeed {
            token_hash: Some(token_hash),
            owner_id,
            display_name: normalize_name(display_name, "Owner")?,
            passkey_name: "Primary passkey".into(),
            recovery_state_id: None,
            kind: "initialize",
            exclude: None,
        })
        .await
    }

    pub async fn initialize_complete(
        &self,
        ceremony_id: &str,
        credential: Value,
    ) -> Result<AuthenticationGrant, IdentityError> {
        let state = self
            .take_registration_state(ceremony_id, "initialize")
            .await?;
        let credential: RegisterPublicKeyCredential =
            serde_json::from_value(credential).map_err(|_| IdentityError::InvalidCredential)?;
        let passkey = self
            .webauthn()?
            .finish_passkey_registration(&credential, &state.state)
            .map_err(|_| IdentityError::InvalidCredential)?;
        let now = Utc::now();
        let session_token = random_token(32);
        let csrf_token = random_token(24);
        let recovery_codes = (0..10).map(|_| random_token(16)).collect::<Vec<_>>();
        let mut session = self.pool.client().start_session().await?;
        session.start_transaction().await?;
        let token_hash = state
            .token_hash
            .as_deref()
            .ok_or(IdentityError::InvalidInitializationToken)?;
        let used_at = format_utc(now);
        let consumed = self
            .pool
            .collection::<Document>("initialization_tokens")
            .update_one(
                doc! {
                    "token_hash": token_hash,
                    "used_at": null,
                    "expires_at": {"$gt": &used_at},
                },
                doc! {"$set": {"used_at": &used_at}},
            )
            .session(&mut session)
            .await?
            .matched_count;
        let owner_count = self
            .pool
            .collection::<Document>("owners")
            .count_documents(doc! {})
            .session(&mut session)
            .await?;
        if consumed != 1 || owner_count != 0 {
            session.abort_transaction().await?;
            return Err(IdentityError::InvalidInitializationToken);
        }
        let created_at = format_utc(now);
        self.pool
            .collection::<Document>("owners")
            .insert_one(doc! {
                "_id": &state.owner_id,
                "display_name": &state.display_name,
                "created_at": &created_at,
            })
            .session(&mut session)
            .await?;
        insert_passkey(
            &self.pool,
            &mut session,
            &state.owner_id,
            &state.passkey_name,
            &passkey,
            now,
        )
        .await?;
        insert_recovery_codes(
            &self.pool,
            &mut session,
            &state.owner_id,
            &recovery_codes,
            now,
        )
        .await?;
        insert_session(
            &self.pool,
            &mut session,
            &state.owner_id,
            &session_token,
            &csrf_token,
            now,
        )
        .await?;
        session.commit_transaction().await?;
        Ok(AuthenticationGrant {
            owner: owner_view(
                &state.owner_id,
                &state.display_name,
                csrf_token.clone(),
                false,
                self.auth_mode,
            ),
            session_token,
            recovery_codes: Some(recovery_codes),
        })
    }

    pub async fn login_options(&self) -> Result<CeremonyOptions, IdentityError> {
        let mut cursor = self
            .pool
            .collection::<Document>("passkeys")
            .find(doc! {"revoked_at": null})
            .sort(doc! {"created_at": 1})
            .await?;
        let mut credential_json = Vec::new();
        while let Some(document) = cursor.try_next().await? {
            credential_json.push(document.get_str("credential_json")?.to_owned());
        }
        if credential_json.is_empty() {
            return Err(IdentityError::AuthRequired);
        }
        let passkeys = credential_json
            .into_iter()
            .map(|raw| serde_json::from_str::<Passkey>(&raw))
            .collect::<Result<Vec<_>, _>>()?;
        let (options, state) = self
            .webauthn()?
            .start_passkey_authentication(&passkeys)
            .map_err(|_| IdentityError::InvalidCredential)?;
        self.store_ceremony(
            "login",
            serde_json::to_string(&AuthenticationState { state })?,
            serde_json::to_value(options)?,
        )
        .await
    }

    pub async fn login_complete(
        &self,
        ceremony_id: &str,
        credential: Value,
    ) -> Result<AuthenticationGrant, IdentityError> {
        let state_json = self.take_ceremony(ceremony_id, "login").await?;
        let state: AuthenticationState = serde_json::from_str(&state_json)?;
        let credential: PublicKeyCredential =
            serde_json::from_value(credential).map_err(|_| IdentityError::InvalidCredential)?;
        let result = self
            .webauthn()?
            .finish_passkey_authentication(&credential, &state.state)
            .map_err(|_| IdentityError::InvalidCredential)?;
        let owner = self.owner().await?;
        let now = Utc::now();
        let mut cursor = self
            .pool
            .collection::<Document>("passkeys")
            .find(doc! {"owner_id": &owner.id, "revoked_at": null})
            .await?;
        let mut matched = None;
        while let Some(document) = cursor.try_next().await? {
            let id = document.get_str("_id")?.to_owned();
            let mut passkey: Passkey = serde_json::from_str(document.get_str("credential_json")?)?;
            if passkey.cred_id() == result.cred_id() {
                let _ = passkey.update_credential(&result);
                matched = Some((id, serde_json::to_string(&passkey)?));
                break;
            }
        }
        let (passkey_id, passkey_json) = matched.ok_or(IdentityError::InvalidCredential)?;
        let session_token = random_token(32);
        let csrf_token = random_token(24);
        let mut session = self.pool.client().start_session().await?;
        session.start_transaction().await?;
        let last_used_at = format_utc(now);
        self.pool
            .collection::<Document>("passkeys")
            .update_one(
                doc! {"_id": &passkey_id},
                doc! {"$set": {"credential_json": &passkey_json, "last_used_at": &last_used_at}},
            )
            .session(&mut session)
            .await?;
        insert_session(
            &self.pool,
            &mut session,
            &owner.id,
            &session_token,
            &csrf_token,
            now,
        )
        .await?;
        session.commit_transaction().await?;
        Ok(AuthenticationGrant {
            owner: owner_view(
                &owner.id,
                &owner.display_name,
                csrf_token,
                false,
                self.auth_mode,
            ),
            session_token,
            recovery_codes: None,
        })
    }

    /// Begin TOTP enrollment: generate a shared secret and store it as a
    /// pending ceremony keyed by the initialization token. The returned
    /// provision is shown to the operator once, and completion requires a code
    /// proving the authenticator was enrolled.
    pub async fn totp_initialize_options(
        &self,
        token: &str,
        display_name: &str,
    ) -> Result<TotpProvision, IdentityError> {
        if self.auth_mode != AuthMode::Totp {
            return Err(IdentityError::TotpDisabled);
        }
        if self.initialization_state().await? == InitializationState::Initialized {
            return Err(IdentityError::AlreadyInitialized);
        }
        let token_hash = purpose_hash("initialization-token", token);
        let now = now_utc_str();
        let valid = self
            .pool
            .collection::<Document>("initialization_tokens")
            .count_documents(doc! {
                "token_hash": &token_hash,
                "used_at": null,
                "expires_at": {"$gt": &now},
            })
            .await?;
        if valid != 1 {
            return Err(IdentityError::InvalidInitializationToken);
        }
        let secret_base32 = base32_encode(&random_bytes(20));
        let owner_id = OwnerId::new().to_string();
        let display_name = normalize_name(display_name, "Owner")?;
        // The label is a URI component; unencoded `&`/`?`/spaces would break
        // the otpauth URI that authenticator apps scan or parse manually.
        let label_input = format!("Janus:{display_name}");
        let label = utf8_percent_encode(&label_input, NON_ALPHANUMERIC);
        let otpauth_uri = format!(
            "otpauth://totp/{label}?secret={secret_base32}&issuer=Janus&algorithm=SHA1&digits=6&period=30"
        );
        let ceremony_id = self
            .store_totp_initialization(&TotpInitializeState {
                token_hash,
                owner_id,
                display_name,
                secret_base32: secret_base32.clone(),
            })
            .await?;
        Ok(TotpProvision {
            ceremony_id,
            secret_base32,
            otpauth_uri,
        })
    }

    pub async fn totp_initialize_complete(
        &self,
        ceremony_id: &str,
        code: &str,
        origin: Option<&str>,
    ) -> Result<AuthenticationGrant, IdentityError> {
        if self.auth_mode != AuthMode::Totp {
            return Err(IdentityError::TotpDisabled);
        }
        self.check_origin(origin)?;
        if !is_totp_code_shape(code) {
            return Err(IdentityError::InvalidTotpCode);
        }
        self.check_rate()?;
        let state_json = self.take_ceremony(ceremony_id, "totp_initialize").await?;
        let state: TotpInitializeState = serde_json::from_str(&state_json)?;
        let secret = base32_decode(&state.secret_base32)?;
        let counter = match verify_totp(&secret, code, Utc::now().timestamp(), None) {
            Some(counter) => counter,
            None => {
                self.record_failure();
                return Err(IdentityError::InvalidTotpCode);
            }
        };
        let now = Utc::now();
        let session_token = random_token(32);
        let csrf_token = random_token(24);
        let recovery_codes = (0..10).map(|_| random_token(16)).collect::<Vec<_>>();
        let mut session = self.pool.client().start_session().await?;
        session.start_transaction().await?;
        let used_at = format_utc(now);
        let consumed = self
            .pool
            .collection::<Document>("initialization_tokens")
            .update_one(
                doc! {
                    "token_hash": &state.token_hash,
                    "used_at": null,
                    "expires_at": {"$gt": &used_at},
                },
                doc! {"$set": {"used_at": &used_at}},
            )
            .session(&mut session)
            .await?
            .matched_count;
        let owner_count = self
            .pool
            .collection::<Document>("owners")
            .count_documents(doc! {})
            .session(&mut session)
            .await?;
        if consumed != 1 || owner_count != 0 {
            session.abort_transaction().await?;
            return Err(IdentityError::InvalidInitializationToken);
        }
        let created_at = format_utc(now);
        self.pool
            .collection::<Document>("owners")
            .insert_one(doc! {
                "_id": &state.owner_id,
                "display_name": &state.display_name,
                "totp_secret": &state.secret_base32,
                "totp_last_counter": counter,
                "created_at": &created_at,
            })
            .session(&mut session)
            .await?;
        insert_recovery_codes(
            &self.pool,
            &mut session,
            &state.owner_id,
            &recovery_codes,
            now,
        )
        .await?;
        insert_session(
            &self.pool,
            &mut session,
            &state.owner_id,
            &session_token,
            &csrf_token,
            now,
        )
        .await?;
        session.commit_transaction().await?;
        Ok(AuthenticationGrant {
            owner: owner_view(
                &state.owner_id,
                &state.display_name,
                csrf_token.clone(),
                false,
                self.auth_mode,
            ),
            session_token,
            recovery_codes: Some(recovery_codes),
        })
    }

    /// Verify a TOTP code against the enrolled owner secret and mint a session.
    /// Replay protection is atomic: the counter update only matches when the
    /// presented code is newer than the last accepted one.
    pub async fn totp_login(
        &self,
        code: &str,
        origin: Option<&str>,
    ) -> Result<AuthenticationGrant, IdentityError> {
        if self.auth_mode != AuthMode::Totp {
            return Err(IdentityError::TotpDisabled);
        }
        self.check_origin(origin)?;
        self.check_rate()?;
        // A 6-digit input is a TOTP code; recovery codes are 22-char base64,
        // so shape cleanly separates the two credential types.
        if is_totp_code_shape(code) {
            self.totp_code_login(code).await
        } else {
            self.recovery_code_login(code).await
        }
    }

    /// Verify a TOTP code and issue a session with replay protection.
    async fn totp_code_login(&self, code: &str) -> Result<AuthenticationGrant, IdentityError> {
        let owner = self.owner().await?;
        let owner_doc = self
            .pool
            .collection::<Document>("owners")
            .find_one(doc! {"_id": &owner.id})
            .await?
            .ok_or(IdentityError::AuthRequired)?;
        let secret_base32 = owner_doc
            .get("totp_secret")
            .and_then(Bson::as_str)
            .ok_or(IdentityError::TotpDisabled)?
            .to_owned();
        let last_counter = owner_doc
            .get("totp_last_counter")
            .and_then(Bson::as_i64)
            .unwrap_or(-1);
        let secret = base32_decode(&secret_base32)?;
        let counter = match verify_totp(&secret, code, Utc::now().timestamp(), Some(last_counter)) {
            Some(counter) => counter,
            None => {
                self.record_failure();
                return Err(IdentityError::InvalidTotpCode);
            }
        };
        let now = Utc::now();
        let session_token = random_token(32);
        let csrf_token = random_token(24);
        let mut session = self.pool.client().start_session().await?;
        session.start_transaction().await?;
        let updated = self
            .pool
            .collection::<Document>("owners")
            .update_one(
                doc! {"_id": &owner.id, "totp_last_counter": {"$lt": counter}},
                doc! {
                    "$set": {
                        "totp_last_counter": counter,
                        "last_totp_at": format_utc(now),
                    }
                },
            )
            .session(&mut session)
            .await?
            .matched_count;
        if updated != 1 {
            session.abort_transaction().await?;
            self.record_failure();
            return Err(IdentityError::InvalidTotpCode);
        }
        insert_session(
            &self.pool,
            &mut session,
            &owner.id,
            &session_token,
            &csrf_token,
            now,
        )
        .await?;
        session.commit_transaction().await?;
        Ok(AuthenticationGrant {
            owner: owner_view(
                &owner.id,
                &owner.display_name,
                csrf_token,
                false,
                self.auth_mode,
            ),
            session_token,
            recovery_codes: None,
        })
    }

    /// Redeem a single-use recovery code for a session directly, instead of
    /// the passkey-rebind path that exchange_recovery enables.
    async fn recovery_code_login(&self, code: &str) -> Result<AuthenticationGrant, IdentityError> {
        let now = Utc::now();
        let mut session = self.pool.client().start_session().await?;
        session.start_transaction().await?;
        let owner_id = match self.consume_recovery_code(code, &mut session).await {
            Ok(owner_id) => owner_id,
            Err(error) => {
                session.abort_transaction().await?;
                self.record_failure();
                return Err(error);
            }
        };
        let owner_doc = self
            .pool
            .collection::<Document>("owners")
            .find_one(doc! {"_id": &owner_id})
            .session(&mut session)
            .await?
            .ok_or(IdentityError::AuthRequired)?;
        let owner = OwnerRow::from_doc(&owner_doc)?;
        let session_token = random_token(32);
        let csrf_token = random_token(24);
        insert_session(
            &self.pool,
            &mut session,
            &owner.id,
            &session_token,
            &csrf_token,
            now,
        )
        .await?;
        session.commit_transaction().await?;
        Ok(AuthenticationGrant {
            owner: owner_view(
                &owner.id,
                &owner.display_name,
                csrf_token,
                false,
                self.auth_mode,
            ),
            session_token,
            recovery_codes: None,
        })
    }

    pub async fn authenticate(
        &self,
        session_token: Option<&str>,
    ) -> Result<AuthContext, IdentityError> {
        if self.development_auth {
            self.ensure_development_owner().await?;
            let owner = self.owner().await?;
            return Ok(AuthContext {
                owner_id: owner.id,
                display_name: owner.display_name,
                session_id: None,
                csrf_token: "development".into(),
                development: true,
            });
        }
        let token = session_token.ok_or(IdentityError::AuthRequired)?;
        let token_hash = purpose_hash("login-session", token);
        let login = self
            .pool
            .collection::<Document>("login_sessions")
            .find_one(doc! {"token_hash": &token_hash, "revoked_at": null})
            .await?
            .ok_or(IdentityError::AuthRequired)?;
        let owner = self
            .pool
            .collection::<Document>("owners")
            .find_one(doc! {"_id": login.get_str("owner_id")?})
            .await?
            .ok_or(IdentityError::AuthRequired)?;
        let expires_at = login.get_str("expires_at")?;
        if parse_time(expires_at)? <= Utc::now() {
            return Err(IdentityError::AuthRequired);
        }
        Ok(AuthContext {
            owner_id: login.get_str("owner_id")?.to_owned(),
            display_name: owner.get_str("display_name")?.to_owned(),
            session_id: Some(login.get_str("_id")?.to_owned()),
            csrf_token: login.get_str("csrf_token")?.to_owned(),
            development: false,
        })
    }

    pub fn authorize_mutation(
        &self,
        auth: &AuthContext,
        origin: Option<&str>,
        csrf_token: Option<&str>,
    ) -> Result<(), IdentityError> {
        if auth.development {
            return Ok(());
        }
        if origin != Some(self.origin.as_str()) {
            return Err(IdentityError::OriginRejected);
        }
        let csrf = csrf_token.ok_or(IdentityError::CsrfRejected)?;
        if csrf != auth.csrf_token {
            return Err(IdentityError::CsrfRejected);
        }
        Ok(())
    }

    pub async fn me(&self, auth: &AuthContext, csrf_token: &str) -> OwnerView {
        owner_view(
            &auth.owner_id,
            &auth.display_name,
            csrf_token.into(),
            auth.development,
            self.auth_mode,
        )
    }

    /// Return the single deployment owner's id for internal automation flows.
    ///
    /// Janus deliberately has no membership model, so an authenticated
    /// webhook does not need to select a tenant or user.
    pub async fn single_owner_id(&self) -> Result<String, IdentityError> {
        if self.development_auth {
            self.ensure_development_owner().await?;
        }
        Ok(self.owner().await?.id)
    }

    pub async fn logout(&self, auth: &AuthContext) -> Result<(), IdentityError> {
        if let Some(session_id) = &auth.session_id {
            let revoked_at = now_utc_str();
            self.pool
                .collection::<Document>("login_sessions")
                .update_one(
                    doc! {"_id": session_id},
                    doc! {"$set": {"revoked_at": &revoked_at}},
                )
                .await?;
        }
        Ok(())
    }

    pub async fn passkeys(&self, auth: &AuthContext) -> Result<Vec<PasskeyView>, IdentityError> {
        let mut cursor = self
            .pool
            .collection::<Document>("passkeys")
            .find(doc! {"owner_id": &auth.owner_id, "revoked_at": null})
            .sort(doc! {"created_at": 1})
            .await?;
        let mut views = Vec::new();
        while let Some(document) = cursor.try_next().await? {
            views.push(PasskeyView {
                id: document.get_str("_id")?.to_owned(),
                name: document.get_str("name")?.to_owned(),
                created_at: document.get_str("created_at")?.to_owned(),
                last_used_at: document
                    .get("last_used_at")
                    .and_then(Bson::as_str)
                    .map(str::to_owned),
            });
        }
        Ok(views)
    }

    pub async fn add_passkey_options(
        &self,
        auth: &AuthContext,
        name: &str,
    ) -> Result<CeremonyOptions, IdentityError> {
        let credentials = self.credential_ids(&auth.owner_id).await?;
        self.registration_options(RegistrationStateSeed {
            token_hash: None,
            owner_id: auth.owner_id.clone(),
            display_name: auth.display_name.clone(),
            passkey_name: normalize_name(name, "Passkey")?,
            recovery_state_id: None,
            kind: "add_passkey",
            exclude: Some(credentials),
        })
        .await
    }

    pub async fn add_passkey_complete(
        &self,
        auth: &AuthContext,
        ceremony_id: &str,
        credential: Value,
    ) -> Result<PasskeyView, IdentityError> {
        let state = self
            .take_registration_state(ceremony_id, "add_passkey")
            .await?;
        if state.owner_id != auth.owner_id {
            return Err(IdentityError::InvalidCeremony);
        }
        let credential: RegisterPublicKeyCredential =
            serde_json::from_value(credential).map_err(|_| IdentityError::InvalidCredential)?;
        let passkey = self
            .webauthn()?
            .finish_passkey_registration(&credential, &state.state)
            .map_err(|_| IdentityError::InvalidCredential)?;
        let now = Utc::now();
        let id = insert_passkey_pool(
            &self.pool,
            &auth.owner_id,
            &state.passkey_name,
            &passkey,
            now,
        )
        .await?;
        Ok(PasskeyView {
            id,
            name: state.passkey_name,
            created_at: format_utc(now),
            last_used_at: None,
        })
    }

    pub async fn rename_passkey(
        &self,
        auth: &AuthContext,
        id: &str,
        name: &str,
    ) -> Result<(), IdentityError> {
        let name = normalize_name(name, "Passkey")?;
        let changed = self
            .pool
            .collection::<Document>("passkeys")
            .update_one(
                doc! {"_id": id, "owner_id": &auth.owner_id, "revoked_at": null},
                doc! {"$set": {"name": &name}},
            )
            .await?
            .matched_count;
        if changed == 0 {
            Err(IdentityError::PasskeyNotFound)
        } else {
            Ok(())
        }
    }

    pub async fn revoke_passkey(&self, auth: &AuthContext, id: &str) -> Result<(), IdentityError> {
        let mut session = self.pool.client().start_session().await?;
        session.start_transaction().await?;
        let count = self
            .pool
            .collection::<Document>("passkeys")
            .count_documents(doc! {"owner_id": &auth.owner_id, "revoked_at": null})
            .session(&mut session)
            .await?;
        if count <= 1 {
            session.abort_transaction().await?;
            return Err(IdentityError::LastPasskey);
        }
        let revoked_at = now_utc_str();
        let changed = self
            .pool
            .collection::<Document>("passkeys")
            .update_one(
                doc! {"_id": id, "owner_id": &auth.owner_id, "revoked_at": null},
                doc! {"$set": {"revoked_at": &revoked_at}},
            )
            .session(&mut session)
            .await?
            .matched_count;
        if changed == 0 {
            session.abort_transaction().await?;
            return Err(IdentityError::PasskeyNotFound);
        }
        session.commit_transaction().await?;
        Ok(())
    }

    pub async fn regenerate_recovery_codes(
        &self,
        auth: &AuthContext,
    ) -> Result<Vec<String>, IdentityError> {
        let codes = (0..10).map(|_| random_token(16)).collect::<Vec<_>>();
        let now = Utc::now();
        let mut session = self.pool.client().start_session().await?;
        session.start_transaction().await?;
        let revoked_at = format_utc(now);
        self.pool
            .collection::<Document>("recovery_batches")
            .update_many(
                doc! {"owner_id": &auth.owner_id, "revoked_at": null},
                doc! {"$set": {"revoked_at": &revoked_at}},
            )
            .session(&mut session)
            .await?;
        insert_recovery_codes(&self.pool, &mut session, &auth.owner_id, &codes, now).await?;
        session.commit_transaction().await?;
        Ok(codes)
    }

    pub async fn exchange_recovery(
        &self,
        code_or_token: &str,
    ) -> Result<RecoveryGrant, IdentityError> {
        let now = Utc::now();
        let token_hash = purpose_hash("recovery-state", code_or_token);
        let now_str = format_utc(now);
        let existing = self
            .pool
            .collection::<Document>("recovery_states")
            .find_one(doc! {
                "token_hash": &token_hash,
                "used_at": null,
                "expires_at": {"$gt": &now_str},
            })
            .await?;
        if let Some(document) = existing {
            return Ok(RecoveryGrant {
                token: code_or_token.into(),
                expires_at: document.get_str("expires_at")?.to_owned(),
            });
        }
        let mut session = self.pool.client().start_session().await?;
        session.start_transaction().await?;
        let owner_id = self
            .consume_recovery_code(code_or_token, &mut session)
            .await?;
        let token = random_token(32);
        let id = Uuid::now_v7().to_string();
        let token_hash = purpose_hash("recovery-state", &token);
        let expires_at = format_utc(now + RECOVERY_TTL);
        self.pool
            .collection::<Document>("recovery_states")
            .insert_one(doc! {
                "_id": &id,
                "owner_id": &owner_id,
                "token_hash": &token_hash,
                "expires_at": &expires_at,
            })
            .session(&mut session)
            .await?;
        session.commit_transaction().await?;
        Ok(RecoveryGrant { token, expires_at })
    }

    /// Find an unused recovery code, resolve its owner, and mark it used.
    /// Runs inside the caller's transaction so consumption is atomic with the
    /// caller's follow-up write (a recovery-state token or a session).
    async fn consume_recovery_code(
        &self,
        code: &str,
        session: &mut ClientSession,
    ) -> Result<String, IdentityError> {
        let now = Utc::now();
        let code_hash = purpose_hash("recovery-code", code);
        let recovery_code = self
            .pool
            .collection::<Document>("recovery_codes")
            .find_one(doc! {"code_hash": &code_hash, "used_at": null})
            .session(&mut *session)
            .await?
            .ok_or(IdentityError::InvalidRecoveryCode)?;
        let code_id = recovery_code.get_str("_id")?.to_owned();
        let batch_id = recovery_code.get_str("batch_id")?.to_owned();
        let batch = self
            .pool
            .collection::<Document>("recovery_batches")
            .find_one(doc! {"_id": &batch_id, "revoked_at": null})
            .session(&mut *session)
            .await?
            .ok_or(IdentityError::InvalidRecoveryCode)?;
        let owner_id = batch.get_str("owner_id")?.to_owned();
        let consumed = self
            .pool
            .collection::<Document>("recovery_codes")
            .update_one(
                doc! {"_id": &code_id, "used_at": null},
                doc! {"$set": {"used_at": format_utc(now)}},
            )
            .session(&mut *session)
            .await?
            .matched_count;
        if consumed != 1 {
            return Err(IdentityError::InvalidRecoveryCode);
        }
        Ok(owner_id)
    }

    pub async fn recovery_passkey_options(
        &self,
        recovery_token: &str,
        name: &str,
    ) -> Result<CeremonyOptions, IdentityError> {
        let token_hash = purpose_hash("recovery-state", recovery_token);
        let now = now_utc_str();
        let recovery = self
            .pool
            .collection::<Document>("recovery_states")
            .find_one(doc! {
                "token_hash": &token_hash,
                "used_at": null,
                "expires_at": {"$gt": &now},
            })
            .await?
            .ok_or(IdentityError::InvalidRecoveryCode)?;
        let owner_id = recovery.get_str("owner_id")?.to_owned();
        let display_name = self
            .pool
            .collection::<Document>("owners")
            .find_one(doc! {"_id": &owner_id})
            .await?
            .map(|document| document.get_str("display_name").map(str::to_owned))
            .transpose()?
            .unwrap_or_else(|| "Owner".to_owned());
        let credentials = self.credential_ids(&owner_id).await?;
        self.registration_options(RegistrationStateSeed {
            token_hash: None,
            owner_id,
            display_name,
            passkey_name: normalize_name(name, "Recovered passkey")?,
            recovery_state_id: Some(recovery.get_str("_id")?.to_owned()),
            kind: "recovery_passkey",
            exclude: Some(credentials),
        })
        .await
    }

    pub async fn recovery_passkey_complete(
        &self,
        recovery_token: &str,
        ceremony_id: &str,
        credential: Value,
    ) -> Result<AuthenticationGrant, IdentityError> {
        let state = self
            .take_registration_state(ceremony_id, "recovery_passkey")
            .await?;
        let recovery_id = state
            .recovery_state_id
            .as_deref()
            .ok_or(IdentityError::InvalidCeremony)?;
        let credential: RegisterPublicKeyCredential =
            serde_json::from_value(credential).map_err(|_| IdentityError::InvalidCredential)?;
        let passkey = self
            .webauthn()?
            .finish_passkey_registration(&credential, &state.state)
            .map_err(|_| IdentityError::InvalidCredential)?;
        let now = Utc::now();
        let session_token = random_token(32);
        let csrf_token = random_token(24);
        let owner = self.owner().await?;
        let mut session = self.pool.client().start_session().await?;
        session.start_transaction().await?;
        let token_hash = purpose_hash("recovery-state", recovery_token);
        let used_at = format_utc(now);
        let consumed = self
            .pool
            .collection::<Document>("recovery_states")
            .update_one(
                doc! {
                    "_id": recovery_id,
                    "owner_id": &state.owner_id,
                    "token_hash": &token_hash,
                    "used_at": null,
                    "expires_at": {"$gt": &used_at},
                },
                doc! {"$set": {"used_at": &used_at}},
            )
            .session(&mut session)
            .await?
            .matched_count;
        if consumed != 1 {
            session.abort_transaction().await?;
            return Err(IdentityError::InvalidRecoveryCode);
        }
        insert_passkey(
            &self.pool,
            &mut session,
            &state.owner_id,
            &state.passkey_name,
            &passkey,
            now,
        )
        .await?;
        insert_session(
            &self.pool,
            &mut session,
            &state.owner_id,
            &session_token,
            &csrf_token,
            now,
        )
        .await?;
        session.commit_transaction().await?;
        Ok(AuthenticationGrant {
            owner: owner_view(
                &owner.id,
                &owner.display_name,
                csrf_token,
                false,
                self.auth_mode,
            ),
            session_token,
            recovery_codes: None,
        })
    }

    async fn registration_options(
        &self,
        seed: RegistrationStateSeed,
    ) -> Result<CeremonyOptions, IdentityError> {
        let user_id = Uuid::parse_str(&seed.owner_id)
            .map_err(|error| IdentityError::Internal(error.into()))?;
        let (options, state) = self
            .webauthn()?
            .start_passkey_registration(
                user_id,
                "owner@janus.local",
                &seed.display_name,
                seed.exclude,
            )
            .map_err(|_| IdentityError::InvalidCredential)?;
        let stored = RegistrationState {
            token_hash: seed.token_hash,
            owner_id: seed.owner_id,
            display_name: seed.display_name,
            passkey_name: seed.passkey_name,
            recovery_state_id: seed.recovery_state_id,
            state,
        };
        self.store_ceremony(
            seed.kind,
            serde_json::to_string(&stored)?,
            serde_json::to_value(options)?,
        )
        .await
    }

    async fn store_ceremony(
        &self,
        kind: &str,
        state_json: String,
        options: Value,
    ) -> Result<CeremonyOptions, IdentityError> {
        let id = Uuid::now_v7().to_string();
        let now = Utc::now();
        let expires_at = format_utc(now + CEREMONY_TTL);
        let created_at = format_utc(now);
        self.pool
            .collection::<Document>("ceremonies")
            .insert_one(doc! {
                "_id": &id,
                "kind": kind,
                "state_json": &state_json,
                "expires_at": &expires_at,
                "created_at": &created_at,
            })
            .await?;
        Ok(CeremonyOptions {
            ceremony_id: id,
            public_key: options,
        })
    }

    async fn take_registration_state(
        &self,
        id: &str,
        kind: &str,
    ) -> Result<RegistrationState, IdentityError> {
        Ok(serde_json::from_str(&self.take_ceremony(id, kind).await?)?)
    }

    async fn take_ceremony(&self, id: &str, kind: &str) -> Result<String, IdentityError> {
        let now = now_utc_str();
        let mut session = self.pool.client().start_session().await?;
        session.start_transaction().await?;
        let state = self
            .pool
            .collection::<Document>("ceremonies")
            .find_one(doc! {
                "_id": id,
                "kind": kind,
                "consumed_at": null,
                "expires_at": {"$gt": &now},
            })
            .session(&mut session)
            .await?
            .ok_or(IdentityError::InvalidCeremony)?
            .get_str("state_json")?
            .to_owned();
        let changed = self
            .pool
            .collection::<Document>("ceremonies")
            .update_one(
                doc! {"_id": id, "consumed_at": null},
                doc! {"$set": {"consumed_at": &now}},
            )
            .session(&mut session)
            .await?
            .matched_count;
        if changed != 1 {
            session.abort_transaction().await?;
            return Err(IdentityError::InvalidCeremony);
        }
        session.commit_transaction().await?;
        Ok(state)
    }

    async fn owner(&self) -> Result<OwnerRow, IdentityError> {
        self.pool
            .collection::<Document>("owners")
            .find_one(doc! {})
            .await?
            .map(|document| OwnerRow::from_doc(&document))
            .transpose()?
            .ok_or(IdentityError::AuthRequired)
    }

    async fn credential_ids(
        &self,
        owner_id: &str,
    ) -> Result<Vec<webauthn_rs::prelude::CredentialID>, IdentityError> {
        let mut cursor = self
            .pool
            .collection::<Document>("passkeys")
            .find(doc! {"owner_id": owner_id, "revoked_at": null})
            .await?;
        let mut credentials = Vec::new();
        while let Some(document) = cursor.try_next().await? {
            let passkey: Passkey = serde_json::from_str(document.get_str("credential_json")?)?;
            credentials.push(passkey.cred_id().clone());
        }
        Ok(credentials)
    }

    async fn ensure_development_owner(&self) -> Result<(), IdentityError> {
        if self.initialization_state().await? == InitializationState::Initialized {
            return Ok(());
        }
        let now = now_utc_str();
        let owner_id = OwnerId::new().to_string();
        let mut session = self.pool.client().start_session().await?;
        session.start_transaction().await?;
        self.pool
            .collection::<Document>("owners")
            .insert_one(doc! {
                "_id": &owner_id,
                "display_name": "Development Owner",
                "created_at": &now,
            })
            .session(&mut session)
            .await?;
        session.commit_transaction().await?;
        Ok(())
    }

    /// Reject a request whose Origin header, when present, does not match the
    /// configured public origin. Unlike `authorize_mutation` this tolerates a
    /// missing header so API clients can log in without a browser.
    fn check_origin(&self, origin: Option<&str>) -> Result<(), IdentityError> {
        if let Some(origin) = origin
            && origin != self.origin
        {
            return Err(IdentityError::OriginRejected);
        }
        Ok(())
    }

    fn check_rate(&self) -> Result<(), IdentityError> {
        let mut rate = self
            .rate
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !rate.allow() {
            return Err(IdentityError::RateLimited);
        }
        Ok(())
    }

    fn record_failure(&self) {
        let mut rate = self
            .rate
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        rate.record();
    }

    async fn store_totp_initialization(
        &self,
        state: &TotpInitializeState,
    ) -> Result<String, IdentityError> {
        let id = Uuid::now_v7().to_string();
        let now = Utc::now();
        let expires_at = format_utc(now + TOTP_INIT_TTL);
        let created_at = format_utc(now);
        let state_json = serde_json::to_string(state)?;
        self.pool
            .collection::<Document>("ceremonies")
            .insert_one(doc! {
                "_id": &id,
                "kind": "totp_initialize",
                "state_json": &state_json,
                "expires_at": &expires_at,
                "created_at": &created_at,
            })
            .await?;
        Ok(id)
    }
}

struct RegistrationStateSeed {
    token_hash: Option<String>,
    owner_id: String,
    display_name: String,
    passkey_name: String,
    recovery_state_id: Option<String>,
    kind: &'static str,
    exclude: Option<Vec<webauthn_rs::prelude::CredentialID>>,
}

fn normalize_name(value: &str, fallback: &str) -> Result<String, IdentityError> {
    let value = value.trim();
    if value.len() > 80 {
        return Err(IdentityError::InvalidCredential);
    }
    Ok(if value.is_empty() {
        fallback.into()
    } else {
        value.into()
    })
}

fn owner_view(
    id: &str,
    display_name: &str,
    csrf_token: String,
    development: bool,
    auth_mode: AuthMode,
) -> OwnerView {
    OwnerView {
        id: id.into(),
        display_name: display_name.into(),
        authentication_mode: if development {
            AuthenticationMode::Development
        } else if auth_mode == AuthMode::Totp {
            AuthenticationMode::Totp
        } else {
            AuthenticationMode::Passkey
        },
        csrf_token,
    }
}

async fn insert_passkey(
    pool: &Database,
    session: &mut ClientSession,
    owner_id: &str,
    name: &str,
    passkey: &Passkey,
    now: DateTime<Utc>,
) -> Result<String, IdentityError> {
    let id = PasskeyId::new().to_string();
    let credential_json = serde_json::to_string(passkey)?;
    let created_at = format_utc(now);
    pool.collection::<Document>("passkeys")
        .insert_one(doc! {
            "_id": &id,
            "owner_id": owner_id,
            "name": name,
            "credential_json": &credential_json,
            "created_at": &created_at,
        })
        .session(&mut *session)
        .await?;
    Ok(id)
}

async fn insert_passkey_pool(
    pool: &Database,
    owner_id: &str,
    name: &str,
    passkey: &Passkey,
    now: DateTime<Utc>,
) -> Result<String, IdentityError> {
    let mut session = pool.client().start_session().await?;
    session.start_transaction().await?;
    let id = insert_passkey(pool, &mut session, owner_id, name, passkey, now).await?;
    session.commit_transaction().await?;
    Ok(id)
}

async fn insert_recovery_codes(
    pool: &Database,
    session: &mut ClientSession,
    owner_id: &str,
    codes: &[String],
    now: DateTime<Utc>,
) -> Result<(), IdentityError> {
    let batch_id = Uuid::now_v7().to_string();
    let created_at = format_utc(now);
    pool.collection::<Document>("recovery_batches")
        .insert_one(doc! {
            "_id": &batch_id,
            "owner_id": owner_id,
            "created_at": &created_at,
        })
        .session(&mut *session)
        .await?;
    for code in codes {
        let id = Uuid::now_v7().to_string();
        let code_hash = purpose_hash("recovery-code", code);
        pool.collection::<Document>("recovery_codes")
            .insert_one(doc! {
                "_id": &id,
                "batch_id": &batch_id,
                "code_hash": &code_hash,
            })
            .session(&mut *session)
            .await?;
    }
    Ok(())
}

async fn insert_session(
    pool: &Database,
    session: &mut ClientSession,
    owner_id: &str,
    token: &str,
    csrf: &str,
    now: DateTime<Utc>,
) -> Result<(), IdentityError> {
    let id = Uuid::now_v7().to_string();
    let token_hash = purpose_hash("login-session", token);
    let created_at = format_utc(now);
    let last_seen_at = format_utc(now);
    let expires_at = format_utc(now + SESSION_IDLE_TTL);
    pool.collection::<Document>("login_sessions")
        .insert_one(doc! {
            "_id": &id,
            "owner_id": owner_id,
            "token_hash": &token_hash,
            "csrf_token": csrf,
            "created_at": &created_at,
            "last_seen_at": &last_seen_at,
            "expires_at": &expires_at,
        })
        .session(&mut *session)
        .await?;
    Ok(())
}

fn parse_time(value: &str) -> Result<DateTime<Utc>, IdentityError> {
    Ok(DateTime::parse_from_rfc3339(value)
        .map_err(|error| IdentityError::Internal(error.into()))?
        .with_timezone(&Utc))
}

/// RFC 4648 base32 encoder without padding, used for the TOTP shared secret.
fn base32_encode(input: &[u8]) -> String {
    const ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
    let mut output = String::new();
    let mut buffer: u32 = 0;
    let mut bits = 0;
    for &byte in input {
        buffer = (buffer << 8) | u32::from(byte);
        bits += 8;
        while bits >= 5 {
            let index = ((buffer >> (bits - 5)) & 0x1f) as usize;
            output.push(ALPHABET[index] as char);
            bits -= 5;
        }
    }
    if bits > 0 {
        let index = ((buffer << (5 - bits)) & 0x1f) as usize;
        output.push(ALPHABET[index] as char);
    }
    output
}

/// RFC 4648 base32 decoder that tolerates optional `=` padding and whitespace.
fn base32_decode(input: &str) -> Result<Vec<u8>, IdentityError> {
    let mut output = Vec::new();
    let mut buffer: u32 = 0;
    let mut bits = 0;
    for ch in input.chars() {
        let value = match ch {
            'A'..='Z' => ch as u8 - b'A',
            'a'..='z' => ch as u8 - b'a',
            '2'..='7' => ch as u8 - b'2' + 26,
            '=' | ' ' | '\t' | '\r' | '\n' | '-' => continue,
            _ => return Err(IdentityError::InvalidTotpCode),
        };
        buffer = (buffer << 5) | u32::from(value);
        bits += 5;
        if bits >= 8 {
            output.push(((buffer >> (bits - 8)) & 0xff) as u8);
            bits -= 8;
        }
    }
    Ok(output)
}

/// HMAC-SHA1 built from the `sha1` crate's digest API, so TOTP does not pull a
/// dedicated HMAC crate into the dependency tree.
fn hmac_sha1(key: &[u8], data: &[u8]) -> Vec<u8> {
    const BLOCK: usize = 64;
    let mut block_key = [0_u8; BLOCK];
    if key.len() > BLOCK {
        let mut hasher = Sha1::new();
        hasher.update(key);
        block_key[..20].copy_from_slice(hasher.finalize().as_ref());
    } else {
        block_key[..key.len()].copy_from_slice(key);
    }
    let mut ipad = [0x36_u8; BLOCK];
    let mut opad = [0x5c_u8; BLOCK];
    for (i, (ipad_byte, opad_byte)) in ipad.iter_mut().zip(opad.iter_mut()).enumerate() {
        *ipad_byte ^= block_key[i];
        *opad_byte ^= block_key[i];
    }
    let mut inner = Sha1::new();
    inner.update(ipad);
    inner.update(data);
    let inner_hash = inner.finalize();
    let mut outer = Sha1::new();
    outer.update(opad);
    outer.update(inner_hash.as_slice());
    outer.finalize().as_slice().to_vec()
}

/// RFC 6238 TOTP code for a counter (unix seconds / 30).
fn totp_code(secret: &[u8], counter: u64) -> u32 {
    let digest = hmac_sha1(secret, &counter.to_be_bytes());
    let offset = (digest[19] & 0x0f) as usize;
    let binary = (u32::from(digest[offset] & 0x7f) << 24)
        | (u32::from(digest[offset + 1]) << 16)
        | (u32::from(digest[offset + 2]) << 8)
        | u32::from(digest[offset + 3]);
    binary % 1_000_000
}

/// Verify a 6-digit code against the current 30-second window plus one window
/// of clock skew. Returns the matched counter so callers can enforce replay
/// protection; `min_counter` rejects codes already accepted.
fn verify_totp(secret: &[u8], code: &str, now_secs: i64, min_counter: Option<i64>) -> Option<i64> {
    let window = now_secs.div_euclid(TOTP_WINDOW_SECS);
    for candidate in [window - 1, window, window + 1] {
        if candidate < 0 {
            continue;
        }
        if let Some(minimum) = min_counter
            && candidate <= minimum
        {
            continue;
        }
        let expected = format!("{:06}", totp_code(secret, candidate as u64));
        if constant_time_eq(code.as_bytes(), expected.as_bytes()) {
            return Some(candidate);
        }
    }
    None
}

fn is_totp_code_shape(code: &str) -> bool {
    code.len() == 6 && code.bytes().all(|byte| byte.is_ascii_digit())
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut diff = 0_u8;
    for (a, b) in left.iter().zip(right) {
        diff |= a ^ b;
    }
    diff == 0
}
