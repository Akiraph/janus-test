use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
};
use janus_identity::{AuthContext, AuthenticationGrant, IdentityError};

use crate::{
    AppState,
    transport::http::{
        dto::{
            CeremonyCompleteRequest, DataResponse, InitializeOptionsRequest, PasskeyOptionsRequest,
            RecoveryExchangeRequest, RenamePasskeyRequest, TotpCodeRequest, TotpLoginRequest,
        },
        problem::Problem,
    },
};

#[utoipa::path(post, path = "/api/v1/auth/initialize/options", request_body = InitializeOptionsRequest, responses((status = 200, body = DataResponse<janus_identity::CeremonyOptions>), (status = 400, body = Problem)))]
pub async fn initialize_options(
    State(state): State<AppState>,
    Json(input): Json<InitializeOptionsRequest>,
) -> Result<Json<DataResponse<janus_identity::CeremonyOptions>>, Problem> {
    Ok(Json(DataResponse {
        data: state
            .identity()
            .initialize_options(&input.initialization_token, &input.display_name)
            .await
            .map_err(problem)?,
    }))
}

#[utoipa::path(post, path = "/api/v1/auth/initialize/complete", request_body = CeremonyCompleteRequest, responses((status = 200, body = DataResponse<janus_identity::OwnerView>), (status = 400, body = Problem)))]
pub async fn initialize_complete(
    State(state): State<AppState>,
    Json(input): Json<CeremonyCompleteRequest>,
) -> Result<(HeaderMap, Json<DataResponse<janus_identity::OwnerView>>), Problem> {
    grant_response(
        &state,
        state
            .identity()
            .initialize_complete(&input.ceremony_id, input.credential)
            .await
            .map_err(problem)?,
    )
}

#[utoipa::path(post, path = "/api/v1/auth/passkey/options", responses((status = 200, body = DataResponse<janus_identity::CeremonyOptions>), (status = 400, body = Problem)))]
pub async fn login_options(
    State(state): State<AppState>,
) -> Result<Json<DataResponse<janus_identity::CeremonyOptions>>, Problem> {
    Ok(Json(DataResponse {
        data: state.identity().login_options().await.map_err(problem)?,
    }))
}
#[utoipa::path(post, path = "/api/v1/auth/passkey/complete", request_body = CeremonyCompleteRequest, responses((status = 200, body = DataResponse<janus_identity::OwnerView>), (status = 400, body = Problem)))]
pub async fn login_complete(
    State(state): State<AppState>,
    Json(input): Json<CeremonyCompleteRequest>,
) -> Result<(HeaderMap, Json<DataResponse<janus_identity::OwnerView>>), Problem> {
    grant_response(
        &state,
        state
            .identity()
            .login_complete(&input.ceremony_id, input.credential)
            .await
            .map_err(problem)?,
    )
}

#[utoipa::path(get, path = "/api/v1/me", responses((status = 200, body = DataResponse<janus_identity::OwnerView>), (status = 401, body = Problem)))]
pub async fn me(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<DataResponse<janus_identity::OwnerView>>, Problem> {
    let auth = authenticate(&state, &headers).await?;
    Ok(Json(DataResponse {
        data: state.identity().me(&auth, &auth.csrf_token).await,
    }))
}
#[utoipa::path(post, path = "/api/v1/auth/logout", responses((status = 204), (status = 401, body = Problem)))]
pub async fn logout(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<(HeaderMap, StatusCode), Problem> {
    let auth = authorized(&state, &headers).await?;
    state.identity().logout(&auth).await.map_err(problem)?;
    let mut output = HeaderMap::new();
    output.insert(
        header::SET_COOKIE,
        HeaderValue::from_str(&session_cookie(&state, "", 0)).map_err(|_| {
            Problem::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "INTERNAL_ERROR",
                "Internal server error",
                "The login cookie could not be cleared.",
            )
        })?,
    );
    Ok((output, StatusCode::NO_CONTENT))
}
#[utoipa::path(get, path = "/api/v1/me/passkeys", responses((status = 200, body = DataResponse<Vec<janus_identity::PasskeyView>>), (status = 401, body = Problem)))]
pub async fn passkeys(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<DataResponse<Vec<janus_identity::PasskeyView>>>, Problem> {
    let auth = authenticate(&state, &headers).await?;
    Ok(Json(DataResponse {
        data: state.identity().passkeys(&auth).await.map_err(problem)?,
    }))
}
#[utoipa::path(post, path = "/api/v1/me/passkeys/options", request_body = PasskeyOptionsRequest, responses((status = 200, body = DataResponse<janus_identity::CeremonyOptions>), (status = 401, body = Problem)))]
pub async fn passkey_options(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(input): Json<PasskeyOptionsRequest>,
) -> Result<Json<DataResponse<janus_identity::CeremonyOptions>>, Problem> {
    let auth = authorized(&state, &headers).await?;
    Ok(Json(DataResponse {
        data: state
            .identity()
            .add_passkey_options(&auth, &input.name)
            .await
            .map_err(problem)?,
    }))
}
#[utoipa::path(post, path = "/api/v1/me/passkeys/complete", request_body = CeremonyCompleteRequest, responses((status = 200, body = DataResponse<janus_identity::PasskeyView>), (status = 401, body = Problem)))]
pub async fn passkey_complete(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(input): Json<CeremonyCompleteRequest>,
) -> Result<Json<DataResponse<janus_identity::PasskeyView>>, Problem> {
    let auth = authorized(&state, &headers).await?;
    Ok(Json(DataResponse {
        data: state
            .identity()
            .add_passkey_complete(&auth, &input.ceremony_id, input.credential)
            .await
            .map_err(problem)?,
    }))
}
#[utoipa::path(patch, path = "/api/v1/me/passkeys/{id}", params(("id" = String, Path)), request_body = RenamePasskeyRequest, responses((status = 204), (status = 401, body = Problem)))]
pub async fn rename_passkey(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(input): Json<RenamePasskeyRequest>,
) -> Result<StatusCode, Problem> {
    let auth = authorized(&state, &headers).await?;
    state
        .identity()
        .rename_passkey(&auth, &id, &input.name)
        .await
        .map_err(problem)?;
    Ok(StatusCode::NO_CONTENT)
}
#[utoipa::path(delete, path = "/api/v1/me/passkeys/{id}", params(("id" = String, Path)), responses((status = 204), (status = 401, body = Problem)))]
pub async fn revoke_passkey(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<StatusCode, Problem> {
    let auth = authorized(&state, &headers).await?;
    state
        .identity()
        .revoke_passkey(&auth, &id)
        .await
        .map_err(problem)?;
    Ok(StatusCode::NO_CONTENT)
}
#[utoipa::path(post, path = "/api/v1/me/recovery-codes/regenerate", responses((status = 200, body = DataResponse<Vec<String>>), (status = 401, body = Problem)))]
pub async fn regenerate_recovery_codes(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<DataResponse<Vec<String>>>, Problem> {
    let auth = authorized(&state, &headers).await?;
    Ok(Json(DataResponse {
        data: state
            .identity()
            .regenerate_recovery_codes(&auth)
            .await
            .map_err(problem)?,
    }))
}
#[utoipa::path(post, path = "/api/v1/auth/recovery/exchange", request_body = RecoveryExchangeRequest, responses((status = 200, body = DataResponse<String>), (status = 400, body = Problem)))]
pub async fn recovery_exchange(
    State(state): State<AppState>,
    Json(input): Json<RecoveryExchangeRequest>,
) -> Result<(HeaderMap, Json<DataResponse<String>>), Problem> {
    let grant = state
        .identity()
        .exchange_recovery(&input.code)
        .await
        .map_err(problem)?;
    let mut headers = HeaderMap::new();
    let secure = match state.config().public_origin.scheme() {
        "https" => "; Secure",
        _ => "",
    };
    let cookie = format!(
        "janus_recovery={}; Path=/api/v1/auth/recovery; Max-Age=600{secure}; HttpOnly; SameSite=Strict",
        grant.token
    );
    headers.insert(
        header::SET_COOKIE,
        HeaderValue::from_str(&cookie).map_err(|_| {
            Problem::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "INTERNAL_ERROR",
                "Internal server error",
                "The recovery cookie could not be created.",
            )
        })?,
    );
    Ok((
        headers,
        Json(DataResponse {
            data: grant.expires_at,
        }),
    ))
}
#[utoipa::path(post, path = "/api/v1/auth/recovery/passkey/options", request_body = PasskeyOptionsRequest, responses((status = 200, body = DataResponse<janus_identity::CeremonyOptions>), (status = 400, body = Problem)))]
pub async fn recovery_passkey_options(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(input): Json<PasskeyOptionsRequest>,
) -> Result<Json<DataResponse<janus_identity::CeremonyOptions>>, Problem> {
    let token = cookie(&headers, "janus_recovery")
        .ok_or_else(|| problem(IdentityError::InvalidRecoveryCode))?;
    Ok(Json(DataResponse {
        data: state
            .identity()
            .recovery_passkey_options(&token, &input.name)
            .await
            .map_err(problem)?,
    }))
}
#[utoipa::path(post, path = "/api/v1/auth/recovery/passkey/complete", request_body = CeremonyCompleteRequest, responses((status = 200, body = DataResponse<janus_identity::OwnerView>), (status = 400, body = Problem)))]
pub async fn recovery_passkey_complete(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(input): Json<CeremonyCompleteRequest>,
) -> Result<(HeaderMap, Json<DataResponse<janus_identity::OwnerView>>), Problem> {
    let token = cookie(&headers, "janus_recovery")
        .ok_or_else(|| problem(IdentityError::InvalidRecoveryCode))?;
    grant_response(
        &state,
        state
            .identity()
            .recovery_passkey_complete(&token, &input.ceremony_id, input.credential)
            .await
            .map_err(problem)?,
    )
}

#[utoipa::path(post, path = "/api/v1/auth/totp/initialize/options", request_body = InitializeOptionsRequest, responses((status = 200, body = DataResponse<janus_identity::TotpProvision>), (status = 400, body = Problem)))]
pub async fn totp_initialize_options(
    State(state): State<AppState>,
    Json(input): Json<InitializeOptionsRequest>,
) -> Result<Json<DataResponse<janus_identity::TotpProvision>>, Problem> {
    Ok(Json(DataResponse {
        data: state
            .identity()
            .totp_initialize_options(&input.initialization_token, &input.display_name)
            .await
            .map_err(problem)?,
    }))
}

#[utoipa::path(post, path = "/api/v1/auth/totp/initialize/complete", request_body = TotpCodeRequest, responses((status = 200, body = DataResponse<janus_identity::OwnerView>), (status = 400, body = Problem)))]
pub async fn totp_initialize_complete(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(input): Json<TotpCodeRequest>,
) -> Result<(HeaderMap, Json<DataResponse<janus_identity::OwnerView>>), Problem> {
    let origin = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok());
    grant_response(
        &state,
        state
            .identity()
            .totp_initialize_complete(&input.ceremony_id, &input.code, origin)
            .await
            .map_err(problem)?,
    )
}

#[utoipa::path(post, path = "/api/v1/auth/totp/login", request_body = TotpLoginRequest, responses((status = 200, body = DataResponse<janus_identity::OwnerView>), (status = 400, body = Problem)))]
pub async fn totp_login(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(input): Json<TotpLoginRequest>,
) -> Result<(HeaderMap, Json<DataResponse<janus_identity::OwnerView>>), Problem> {
    let origin = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok());
    grant_response(
        &state,
        state
            .identity()
            .totp_login(&input.code, origin)
            .await
            .map_err(problem)?,
    )
}

pub async fn authenticate(state: &AppState, headers: &HeaderMap) -> Result<AuthContext, Problem> {
    let name = if state.config().public_origin.scheme() == "https" {
        "__Host-janus_session"
    } else {
        "janus_session"
    };
    let token = cookie(headers, name);
    state
        .identity()
        .authenticate(token.as_deref())
        .await
        .map_err(problem)
}
pub async fn authorized(state: &AppState, headers: &HeaderMap) -> Result<AuthContext, Problem> {
    let auth = authenticate(state, headers).await?;
    state
        .identity()
        .authorize_mutation(
            &auth,
            headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()),
            headers.get("x-csrf-token").and_then(|v| v.to_str().ok()),
        )
        .map_err(problem)?;
    Ok(auth)
}

/// Session cookie name and attributes depend on the origin scheme: browsers
/// refuse `Secure` cookies (and `__Host-` names) over plain http, which is
/// exactly the deployment TOTP mode exists for. https origins keep the
/// hardened name.
fn session_cookie(state: &AppState, token: &str, max_age: u64) -> String {
    let (name, secure) = match state.config().public_origin.scheme() {
        "https" => ("__Host-janus_session", "; Secure"),
        _ => ("janus_session", ""),
    };
    format!("{name}={token}; Path=/; Max-Age={max_age}{secure}; HttpOnly; SameSite=Strict")
}

fn grant_response(
    state: &AppState,
    grant: AuthenticationGrant,
) -> Result<(HeaderMap, Json<DataResponse<janus_identity::OwnerView>>), Problem> {
    let mut headers = HeaderMap::new();
    let cookie = session_cookie(state, &grant.session_token, 604800);
    headers.insert(
        header::SET_COOKIE,
        HeaderValue::from_str(&cookie).map_err(|_| {
            Problem::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "INTERNAL_ERROR",
                "Internal server error",
                "The login cookie could not be created.",
            )
        })?,
    );
    if let Some(codes) = grant.recovery_codes {
        headers.insert(
            "x-janus-recovery-codes",
            HeaderValue::from_str(&serde_json::to_string(&codes).unwrap_or_default()).map_err(
                |_| {
                    Problem::new(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "INTERNAL_ERROR",
                        "Internal server error",
                        "The recovery codes could not be returned.",
                    )
                },
            )?,
        );
    }
    Ok((headers, Json(DataResponse { data: grant.owner })))
}
fn cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(header::COOKIE)?
        .to_str()
        .ok()?
        .split(';')
        .find_map(|part| {
            let (mut key, value) = part.trim().split_once('=')?;
            key = key.trim();
            (key == name).then(|| value.to_owned())
        })
}
fn problem(error: IdentityError) -> Problem {
    match error {
        IdentityError::AuthRequired => Problem::new(
            StatusCode::UNAUTHORIZED,
            "AUTH_REQUIRED",
            "Authentication required",
            error.to_string(),
        ),
        IdentityError::AlreadyInitialized => Problem::new(
            StatusCode::CONFLICT,
            "INITIALIZATION_ALREADY_COMPLETE",
            "Initialization complete",
            error.to_string(),
        ),
        IdentityError::InvalidInitializationToken => Problem::new(
            StatusCode::UNAUTHORIZED,
            "INITIALIZATION_TOKEN_INVALID",
            "Invalid initialization token",
            error.to_string(),
        ),
        IdentityError::InvalidCeremony | IdentityError::InvalidCredential => Problem::new(
            StatusCode::UNAUTHORIZED,
            "WEBAUTHN_VERIFICATION_FAILED",
            "Passkey verification failed",
            error.to_string(),
        ),
        IdentityError::LastPasskey => Problem::new(
            StatusCode::CONFLICT,
            "LAST_PASSKEY",
            "Last passkey",
            error.to_string(),
        ),
        IdentityError::PasskeyNotFound => Problem::new(
            StatusCode::NOT_FOUND,
            "RESOURCE_NOT_FOUND",
            "Passkey not found",
            error.to_string(),
        ),
        IdentityError::OriginRejected => Problem::new(
            StatusCode::FORBIDDEN,
            "ORIGIN_REJECTED",
            "Origin rejected",
            error.to_string(),
        ),
        IdentityError::CsrfRejected => Problem::new(
            StatusCode::FORBIDDEN,
            "CSRF_REJECTED",
            "CSRF rejected",
            error.to_string(),
        ),
        IdentityError::InvalidRecoveryCode => Problem::new(
            StatusCode::UNAUTHORIZED,
            "RECOVERY_CODE_INVALID",
            "Recovery failed",
            error.to_string(),
        ),
        IdentityError::PasskeyDisabled => Problem::new(
            StatusCode::FORBIDDEN,
            "PASSKEY_DISABLED",
            "Passkey authentication is disabled",
            error.to_string(),
        ),
        IdentityError::TotpDisabled => Problem::new(
            StatusCode::FORBIDDEN,
            "TOTP_DISABLED",
            "TOTP authentication is disabled",
            error.to_string(),
        ),
        IdentityError::InvalidTotpCode => Problem::new(
            StatusCode::UNAUTHORIZED,
            "TOTP_CODE_INVALID",
            "TOTP code invalid",
            error.to_string(),
        ),
        IdentityError::RateLimited => Problem::new(
            StatusCode::TOO_MANY_REQUESTS,
            "RATE_LIMITED",
            "Rate limit exceeded",
            error.to_string(),
        ),
        IdentityError::Storage(_)
        | IdentityError::Data(_)
        | IdentityError::Internal(_)
        | IdentityError::ValueAccess(_) => Problem::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "INTERNAL_ERROR",
            "Internal server error",
            "The identity operation could not be completed.",
        ),
    }
}
