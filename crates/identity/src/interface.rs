//! Public identity capability boundary.

use chrono::{DateTime, Duration, Utc};
use janus_infrastructure::clock::{format_utc, now_utc_str};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{FromRow, SqlitePool};
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
    secrets::{purpose_hash, random_token},
};

const CEREMONY_TTL: Duration = Duration::minutes(5);
const SESSION_IDLE_TTL: Duration = Duration::days(7);
const RECOVERY_TTL: Duration = Duration::minutes(10);

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
    Development,
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
    #[error("identity storage failed")]
    Storage(#[from] sqlx::Error),
    #[error("identity data is invalid")]
    Data(#[from] serde_json::Error),
    #[error("identity operation failed")]
    Internal(#[from] anyhow::Error),
}

#[derive(Clone)]
pub struct IdentityInterface {
    pool: SqlitePool,
    webauthn: Webauthn,
    origin: String,
    development_auth: bool,
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

#[derive(FromRow)]
struct OwnerRow {
    id: String,
    display_name: String,
}

#[derive(FromRow)]
struct SessionRow {
    id: String,
    owner_id: String,
    display_name: String,
    csrf_token: String,
    expires_at: String,
}

impl IdentityInterface {
    pub fn new(
        pool: SqlitePool,
        webauthn_rp_id: &str,
        public_origin: &url::Url,
        webauthn_rp_name: &str,
        development_auth: bool,
    ) -> anyhow::Result<Self> {
        let webauthn = WebauthnBuilder::new(webauthn_rp_id, public_origin)?
            .rp_name(webauthn_rp_name)
            .build()?;
        let service = Self {
            pool,
            webauthn,
            origin: public_origin.origin().ascii_serialization(),
            development_auth,
        };
        Ok(service)
    }

    pub async fn initialization_state(&self) -> Result<InitializationState, IdentityError> {
        let count = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM owners")
            .fetch_one(&self.pool)
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
        let mut transaction = self.pool.begin().await?;
        sqlx::query("DELETE FROM initialization_tokens WHERE used_at IS NULL")
            .execute(&mut *transaction)
            .await?;
        sqlx::query("INSERT INTO initialization_tokens (id, token_hash, expires_at, created_at) VALUES (?, ?, ?, ?)")
            .bind(Uuid::now_v7().to_string())
            .bind(purpose_hash("initialization-token", &token))
            .bind(format_utc(now + Duration::minutes(30)))
            .bind(format_utc(now))
            .execute(&mut *transaction)
            .await?;
        transaction.commit().await?;
        Ok(token)
    }

    pub async fn issue_recovery_token(&self) -> Result<String, IdentityError> {
        let owner = self.owner().await?;
        let token = random_token(32);
        let now = Utc::now();
        sqlx::query("INSERT INTO recovery_states (id, owner_id, token_hash, expires_at) VALUES (?, ?, ?, ?)")
            .bind(Uuid::now_v7().to_string()).bind(owner.id).bind(purpose_hash("recovery-state", &token)).bind(format_utc(now + RECOVERY_TTL)).execute(&self.pool).await?;
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
        let valid = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM initialization_tokens WHERE token_hash = ? AND used_at IS NULL AND expires_at > ?",
        )
        .bind(&token_hash)
        .bind(now_utc_str())
        .fetch_one(&self.pool)
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
            .webauthn
            .finish_passkey_registration(&credential, &state.state)
            .map_err(|_| IdentityError::InvalidCredential)?;
        let now = Utc::now();
        let session_token = random_token(32);
        let csrf_token = random_token(24);
        let recovery_codes = (0..10).map(|_| random_token(16)).collect::<Vec<_>>();
        let mut transaction = self.pool.begin().await?;
        let token_hash = state
            .token_hash
            .as_deref()
            .ok_or(IdentityError::InvalidInitializationToken)?;
        let consumed = sqlx::query("UPDATE initialization_tokens SET used_at = ? WHERE token_hash = ? AND used_at IS NULL AND expires_at > ?")
            .bind(format_utc(now))
            .bind(token_hash)
            .bind(format_utc(now))
            .execute(&mut *transaction)
            .await?
            .rows_affected();
        if consumed != 1
            || sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM owners")
                .fetch_one(&mut *transaction)
                .await?
                != 0
        {
            return Err(IdentityError::InvalidInitializationToken);
        }
        sqlx::query(
            "INSERT INTO owners (id, display_name, created_at) VALUES (?, ?, ?)",
        )
        .bind(&state.owner_id)
        .bind(&state.display_name)
        .bind(format_utc(now))
        .execute(&mut *transaction)
        .await?;
        insert_passkey(
            &mut transaction,
            &state.owner_id,
            &state.passkey_name,
            &passkey,
            now,
        )
        .await?;
        insert_recovery_codes(&mut transaction, &state.owner_id, &recovery_codes, now).await?;
        insert_session(
            &mut transaction,
            &state.owner_id,
            &session_token,
            &csrf_token,
            now,
        )
        .await?;
        transaction.commit().await?;
        Ok(AuthenticationGrant {
            owner: owner_view(
                &state.owner_id,
                &state.display_name,
                csrf_token.clone(),
                false,
            ),
            session_token,
            recovery_codes: Some(recovery_codes),
        })
    }

    pub async fn login_options(&self) -> Result<CeremonyOptions, IdentityError> {
        let rows = sqlx::query_as::<_, (String,)>(
            "SELECT credential_json FROM passkeys WHERE revoked_at IS NULL ORDER BY created_at",
        )
        .fetch_all(&self.pool)
        .await?;
        if rows.is_empty() {
            return Err(IdentityError::AuthRequired);
        }
        let passkeys = rows
            .into_iter()
            .map(|row| serde_json::from_str::<Passkey>(&row.0))
            .collect::<Result<Vec<_>, _>>()?;
        let (options, state) = self
            .webauthn
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
            .webauthn
            .finish_passkey_authentication(&credential, &state.state)
            .map_err(|_| IdentityError::InvalidCredential)?;
        let owner = self.owner().await?;
        let now = Utc::now();
        let rows = sqlx::query_as::<_, (String, String)>(
            "SELECT id, credential_json FROM passkeys WHERE owner_id = ? AND revoked_at IS NULL",
        )
        .bind(&owner.id)
        .fetch_all(&self.pool)
        .await?;
        let mut matched = None;
        for (id, credential_json) in rows {
            let mut passkey: Passkey = serde_json::from_str(&credential_json)?;
            if passkey.cred_id() == result.cred_id() {
                let _ = passkey.update_credential(&result);
                matched = Some((id, serde_json::to_string(&passkey)?));
                break;
            }
        }
        let (passkey_id, passkey_json) = matched.ok_or(IdentityError::InvalidCredential)?;
        let session_token = random_token(32);
        let csrf_token = random_token(24);
        let mut transaction = self.pool.begin().await?;
        sqlx::query("UPDATE passkeys SET credential_json = ?, last_used_at = ? WHERE id = ?")
            .bind(passkey_json)
            .bind(format_utc(now))
            .bind(passkey_id)
            .execute(&mut *transaction)
            .await?;
        insert_session(
            &mut transaction,
            &owner.id,
            &session_token,
            &csrf_token,
            now,
        )
        .await?;
        transaction.commit().await?;
        Ok(AuthenticationGrant {
            owner: owner_view(
                &owner.id,
                &owner.display_name,
                csrf_token,
                false,
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
        let row = sqlx::query_as::<_, SessionRow>(
            "SELECT s.id, s.owner_id, o.display_name, s.csrf_token, s.expires_at FROM login_sessions s JOIN owners o ON o.id = s.owner_id WHERE s.token_hash = ? AND s.revoked_at IS NULL",
        )
        .bind(purpose_hash("login-session", token))
        .fetch_optional(&self.pool)
        .await?
        .ok_or(IdentityError::AuthRequired)?;
        if parse_time(&row.expires_at)? <= Utc::now() {
            return Err(IdentityError::AuthRequired);
        }
        Ok(AuthContext {
            owner_id: row.owner_id,
            display_name: row.display_name,
            session_id: Some(row.id),
            csrf_token: row.csrf_token,
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
        )
    }

    pub async fn logout(&self, auth: &AuthContext) -> Result<(), IdentityError> {
        if let Some(session_id) = &auth.session_id {
            sqlx::query("UPDATE login_sessions SET revoked_at = ? WHERE id = ?")
                .bind(now_utc_str())
                .bind(session_id)
                .execute(&self.pool)
                .await?;
        }
        Ok(())
    }

    pub async fn passkeys(&self, auth: &AuthContext) -> Result<Vec<PasskeyView>, IdentityError> {
        let rows = sqlx::query_as::<_, (String, String, String, Option<String>)>(
            "SELECT id, name, created_at, last_used_at FROM passkeys WHERE owner_id = ? AND revoked_at IS NULL ORDER BY created_at",
        )
        .bind(&auth.owner_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|row| PasskeyView {
                id: row.0,
                name: row.1,
                created_at: row.2,
                last_used_at: row.3,
            })
            .collect())
    }

    pub async fn add_passkey_options(
        &self,
        auth: &AuthContext,
        name: &str,
    ) -> Result<CeremonyOptions, IdentityError> {
        let rows = sqlx::query_as::<_, (String,)>(
            "SELECT credential_json FROM passkeys WHERE owner_id = ? AND revoked_at IS NULL",
        )
        .bind(&auth.owner_id)
        .fetch_all(&self.pool)
        .await?;
        let credentials = rows
            .into_iter()
            .map(|row| serde_json::from_str::<Passkey>(&row.0).map(|key| key.cred_id().clone()))
            .collect::<Result<Vec<_>, _>>()?;
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
            .webauthn
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
        let changed = sqlx::query(
            "UPDATE passkeys SET name = ? WHERE id = ? AND owner_id = ? AND revoked_at IS NULL",
        )
        .bind(normalize_name(name, "Passkey")?)
        .bind(id)
        .bind(&auth.owner_id)
        .execute(&self.pool)
        .await?
        .rows_affected();
        if changed == 0 {
            Err(IdentityError::PasskeyNotFound)
        } else {
            Ok(())
        }
    }

    pub async fn revoke_passkey(&self, auth: &AuthContext, id: &str) -> Result<(), IdentityError> {
        let mut transaction = self.pool.begin().await?;
        let count = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM passkeys WHERE owner_id = ? AND revoked_at IS NULL",
        )
        .bind(&auth.owner_id)
        .fetch_one(&mut *transaction)
        .await?;
        if count <= 1 {
            return Err(IdentityError::LastPasskey);
        }
        let changed = sqlx::query("UPDATE passkeys SET revoked_at = ? WHERE id = ? AND owner_id = ? AND revoked_at IS NULL")
            .bind(now_utc_str())
            .bind(id)
            .bind(&auth.owner_id)
            .execute(&mut *transaction)
            .await?
            .rows_affected();
        if changed == 0 {
            return Err(IdentityError::PasskeyNotFound);
        }
        transaction.commit().await?;
        Ok(())
    }

    pub async fn regenerate_recovery_codes(
        &self,
        auth: &AuthContext,
    ) -> Result<Vec<String>, IdentityError> {
        let codes = (0..10).map(|_| random_token(16)).collect::<Vec<_>>();
        let now = Utc::now();
        let mut transaction = self.pool.begin().await?;
        sqlx::query(
            "UPDATE recovery_batches SET revoked_at = ? WHERE owner_id = ? AND revoked_at IS NULL",
        )
        .bind(format_utc(now))
        .bind(&auth.owner_id)
        .execute(&mut *transaction)
        .await?;
        insert_recovery_codes(&mut transaction, &auth.owner_id, &codes, now).await?;
        transaction.commit().await?;
        Ok(codes)
    }

    pub async fn exchange_recovery(
        &self,
        code_or_token: &str,
    ) -> Result<RecoveryGrant, IdentityError> {
        let now = Utc::now();
        let existing = sqlx::query_as::<_, (String, String)>("SELECT id, expires_at FROM recovery_states WHERE token_hash=? AND used_at IS NULL AND expires_at>?")
            .bind(purpose_hash("recovery-state", code_or_token)).bind(format_utc(now)).fetch_optional(&self.pool).await?;
        if let Some((_id, expires_at)) = existing {
            return Ok(RecoveryGrant {
                token: code_or_token.into(),
                expires_at,
            });
        }
        let code_hash = purpose_hash("recovery-code", code_or_token);
        let mut transaction = self.pool.begin().await?;
        let row = sqlx::query_as::<_, (String, String)>("SELECT c.id, b.owner_id FROM recovery_codes c JOIN recovery_batches b ON b.id=c.batch_id WHERE c.code_hash=? AND c.used_at IS NULL AND b.revoked_at IS NULL")
            .bind(code_hash).fetch_optional(&mut *transaction).await?.ok_or(IdentityError::InvalidRecoveryCode)?;
        if sqlx::query("UPDATE recovery_codes SET used_at=? WHERE id=? AND used_at IS NULL")
            .bind(format_utc(now))
            .bind(&row.0)
            .execute(&mut *transaction)
            .await?
            .rows_affected()
            != 1
        {
            return Err(IdentityError::InvalidRecoveryCode);
        }
        let token = random_token(32);
        let expires_at = format_utc(now + RECOVERY_TTL);
        sqlx::query("INSERT INTO recovery_states (id, owner_id, token_hash, expires_at) VALUES (?, ?, ?, ?)").bind(Uuid::now_v7().to_string()).bind(row.1).bind(purpose_hash("recovery-state",&token)).bind(&expires_at).execute(&mut *transaction).await?;
        transaction.commit().await?;
        Ok(RecoveryGrant { token, expires_at })
    }

    pub async fn recovery_passkey_options(
        &self,
        recovery_token: &str,
        name: &str,
    ) -> Result<CeremonyOptions, IdentityError> {
        let row=sqlx::query_as::<_,(String,String,String)>("SELECT r.id,o.id,o.display_name FROM recovery_states r JOIN owners o ON o.id=r.owner_id WHERE r.token_hash=? AND r.used_at IS NULL AND r.expires_at>?")
            .bind(purpose_hash("recovery-state",recovery_token)).bind(now_utc_str()).fetch_optional(&self.pool).await?.ok_or(IdentityError::InvalidRecoveryCode)?;
        let credentials = sqlx::query_as::<_, (String,)>(
            "SELECT credential_json FROM passkeys WHERE owner_id=? AND revoked_at IS NULL",
        )
        .bind(&row.1)
        .fetch_all(&self.pool)
        .await?
        .into_iter()
        .map(|item| serde_json::from_str::<Passkey>(&item.0).map(|key| key.cred_id().clone()))
        .collect::<Result<Vec<_>, _>>()?;
        self.registration_options(RegistrationStateSeed {
            token_hash: None,
            owner_id: row.1,
            display_name: row.2,
            passkey_name: normalize_name(name, "Recovered passkey")?,
            recovery_state_id: Some(row.0),
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
            .webauthn
            .finish_passkey_registration(&credential, &state.state)
            .map_err(|_| IdentityError::InvalidCredential)?;
        let now = Utc::now();
        let session_token = random_token(32);
        let csrf_token = random_token(24);
        let owner = self.owner().await?;
        let mut transaction = self.pool.begin().await?;
        if sqlx::query("UPDATE recovery_states SET used_at=? WHERE id=? AND owner_id=? AND token_hash=? AND used_at IS NULL AND expires_at>?").bind(format_utc(now)).bind(recovery_id).bind(&state.owner_id).bind(purpose_hash("recovery-state",recovery_token)).bind(format_utc(now)).execute(&mut *transaction).await?.rows_affected()!=1{return Err(IdentityError::InvalidRecoveryCode);}
        insert_passkey(
            &mut transaction,
            &state.owner_id,
            &state.passkey_name,
            &passkey,
            now,
        )
        .await?;
        insert_session(
            &mut transaction,
            &state.owner_id,
            &session_token,
            &csrf_token,
            now,
        )
        .await?;
        transaction.commit().await?;
        Ok(AuthenticationGrant {
            owner: owner_view(
                &owner.id,
                &owner.display_name,
                csrf_token,
                false,
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
            .webauthn
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
        sqlx::query("INSERT INTO ceremonies (id, kind, state_json, expires_at, created_at) VALUES (?, ?, ?, ?, ?)")
            .bind(&id)
            .bind(kind)
            .bind(state_json)
            .bind(format_utc(now + CEREMONY_TTL))
            .bind(format_utc(now))
            .execute(&self.pool)
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
        let mut transaction = self.pool.begin().await?;
        let state = sqlx::query_scalar::<_, String>(
            "SELECT state_json FROM ceremonies WHERE id = ? AND kind = ? AND consumed_at IS NULL AND expires_at > ?",
        )
        .bind(id)
        .bind(kind)
        .bind(&now)
        .fetch_optional(&mut *transaction)
        .await?
        .ok_or(IdentityError::InvalidCeremony)?;
        let changed = sqlx::query(
            "UPDATE ceremonies SET consumed_at = ? WHERE id = ? AND consumed_at IS NULL",
        )
        .bind(&now)
        .bind(id)
        .execute(&mut *transaction)
        .await?
        .rows_affected();
        if changed != 1 {
            return Err(IdentityError::InvalidCeremony);
        }
        transaction.commit().await?;
        Ok(state)
    }

    async fn owner(&self) -> Result<OwnerRow, IdentityError> {
        sqlx::query_as::<_, OwnerRow>("SELECT id, display_name FROM owners LIMIT 1")
            .fetch_optional(&self.pool)
            .await?
            .ok_or(IdentityError::AuthRequired)
    }

    async fn ensure_development_owner(&self) -> Result<(), IdentityError> {
        if self.initialization_state().await? == InitializationState::Initialized {
            return Ok(());
        }
        let now = now_utc_str();
        let owner_id = OwnerId::new().to_string();
        let mut transaction = self.pool.begin().await?;
        sqlx::query("INSERT INTO owners (id, display_name, created_at) VALUES (?, 'Development Owner', ?)").bind(owner_id).bind(now).execute(&mut *transaction).await?;
        transaction.commit().await?;
        Ok(())
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
) -> OwnerView {
    OwnerView {
        id: id.into(),
        display_name: display_name.into(),
        authentication_mode: if development {
            AuthenticationMode::Development
        } else {
            AuthenticationMode::Passkey
        },
        csrf_token,
    }
}

async fn insert_passkey(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    owner_id: &str,
    name: &str,
    passkey: &Passkey,
    now: DateTime<Utc>,
) -> Result<String, IdentityError> {
    let id = PasskeyId::new().to_string();
    sqlx::query("INSERT INTO passkeys (id, owner_id, name, credential_json, created_at) VALUES (?, ?, ?, ?, ?)")
        .bind(&id).bind(owner_id).bind(name).bind(serde_json::to_string(passkey)?).bind(format_utc(now))
        .execute(&mut **transaction).await?;
    Ok(id)
}

async fn insert_passkey_pool(
    pool: &SqlitePool,
    owner_id: &str,
    name: &str,
    passkey: &Passkey,
    now: DateTime<Utc>,
) -> Result<String, IdentityError> {
    let mut transaction = pool.begin().await?;
    let id = insert_passkey(&mut transaction, owner_id, name, passkey, now).await?;
    transaction.commit().await?;
    Ok(id)
}

async fn insert_recovery_codes(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    owner_id: &str,
    codes: &[String],
    now: DateTime<Utc>,
) -> Result<(), IdentityError> {
    let batch_id = Uuid::now_v7().to_string();
    sqlx::query("INSERT INTO recovery_batches (id, owner_id, created_at) VALUES (?, ?, ?)")
        .bind(&batch_id)
        .bind(owner_id)
        .bind(format_utc(now))
        .execute(&mut **transaction)
        .await?;
    for code in codes {
        sqlx::query("INSERT INTO recovery_codes (id, batch_id, code_hash) VALUES (?, ?, ?)")
            .bind(Uuid::now_v7().to_string())
            .bind(&batch_id)
            .bind(purpose_hash("recovery-code", code))
            .execute(&mut **transaction)
            .await?;
    }
    Ok(())
}

async fn insert_session(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    owner_id: &str,
    token: &str,
    csrf: &str,
    now: DateTime<Utc>,
) -> Result<(), IdentityError> {
    sqlx::query("INSERT INTO login_sessions (id, owner_id, token_hash, csrf_token, created_at, last_seen_at, expires_at) VALUES (?, ?, ?, ?, ?, ?, ?)")
        .bind(Uuid::now_v7().to_string()).bind(owner_id).bind(purpose_hash("login-session", token)).bind(csrf).bind(format_utc(now)).bind(format_utc(now)).bind(format_utc(now + SESSION_IDLE_TTL)).execute(&mut **transaction).await?;
    Ok(())
}

fn parse_time(value: &str) -> Result<DateTime<Utc>, IdentityError> {
    Ok(DateTime::parse_from_rfc3339(value)
        .map_err(|error| IdentityError::Internal(error.into()))?
        .with_timezone(&Utc))
}
