use axum::{
    Extension, Json,
    extract::{Path, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
};

use crate::{
    AppState,
    modules::identity::interface::{AuthContext, AuthenticationGrant, IdentityError},
    transport::http::{
        dto::{
            CeremonyCompleteRequest, DataResponse, InitializeOptionsRequest, PasskeyOptionsRequest,
            RecoveryExchangeRequest, RenamePasskeyRequest,
        },
        problem::Problem,
        request_id::RequestContext,
    },
};

#[utoipa::path(post, path = "/api/v1/auth/initialize/options", request_body = InitializeOptionsRequest, responses((status = 200, body = DataResponse<crate::modules::identity::interface::CeremonyOptions>), (status = 400, body = Problem)))]
pub async fn initialize_options(
    State(state): State<AppState>,
    Json(input): Json<InitializeOptionsRequest>,
) -> Result<Json<DataResponse<crate::modules::identity::interface::CeremonyOptions>>, Problem> {
    Ok(Json(DataResponse {
        data: state
            .identity()
            .initialize_options(&input.initialization_token, &input.display_name)
            .await
            .map_err(problem)?,
    }))
}

#[utoipa::path(post, path = "/api/v1/auth/initialize/complete", request_body = CeremonyCompleteRequest, responses((status = 200, body = DataResponse<crate::modules::identity::interface::OwnerView>), (status = 400, body = Problem)))]
pub async fn initialize_complete(
    State(state): State<AppState>,
    Json(input): Json<CeremonyCompleteRequest>,
) -> Result<
    (
        HeaderMap,
        Json<DataResponse<crate::modules::identity::interface::OwnerView>>,
    ),
    Problem,
> {
    grant_response(
        state
            .identity()
            .initialize_complete(&input.ceremony_id, input.credential)
            .await
            .map_err(problem)?,
    )
}

#[utoipa::path(post, path = "/api/v1/auth/passkey/options", responses((status = 200, body = DataResponse<crate::modules::identity::interface::CeremonyOptions>), (status = 400, body = Problem)))]
pub async fn login_options(
    State(state): State<AppState>,
) -> Result<Json<DataResponse<crate::modules::identity::interface::CeremonyOptions>>, Problem> {
    Ok(Json(DataResponse {
        data: state.identity().login_options().await.map_err(problem)?,
    }))
}
#[utoipa::path(post, path = "/api/v1/auth/passkey/complete", request_body = CeremonyCompleteRequest, responses((status = 200, body = DataResponse<crate::modules::identity::interface::OwnerView>), (status = 400, body = Problem)))]
pub async fn login_complete(
    State(state): State<AppState>,
    Json(input): Json<CeremonyCompleteRequest>,
) -> Result<
    (
        HeaderMap,
        Json<DataResponse<crate::modules::identity::interface::OwnerView>>,
    ),
    Problem,
> {
    grant_response(
        state
            .identity()
            .login_complete(&input.ceremony_id, input.credential)
            .await
            .map_err(problem)?,
    )
}

#[utoipa::path(get, path = "/api/v1/me", responses((status = 200, body = DataResponse<crate::modules::identity::interface::OwnerView>), (status = 401, body = Problem)))]
pub async fn me(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<DataResponse<crate::modules::identity::interface::OwnerView>>, Problem> {
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
        HeaderValue::from_static(
            "__Host-janus_session=; Path=/; Max-Age=0; Secure; HttpOnly; SameSite=Strict",
        ),
    );
    Ok((output, StatusCode::NO_CONTENT))
}
#[utoipa::path(get, path = "/api/v1/me/passkeys", responses((status = 200, body = DataResponse<Vec<crate::modules::identity::interface::PasskeyView>>), (status = 401, body = Problem)))]
pub async fn passkeys(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<DataResponse<Vec<crate::modules::identity::interface::PasskeyView>>>, Problem> {
    let auth = authenticate(&state, &headers).await?;
    Ok(Json(DataResponse {
        data: state.identity().passkeys(&auth).await.map_err(problem)?,
    }))
}
#[utoipa::path(post, path = "/api/v1/me/passkeys/options", request_body = PasskeyOptionsRequest, responses((status = 200, body = DataResponse<crate::modules::identity::interface::CeremonyOptions>), (status = 401, body = Problem)))]
pub async fn passkey_options(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(input): Json<PasskeyOptionsRequest>,
) -> Result<Json<DataResponse<crate::modules::identity::interface::CeremonyOptions>>, Problem> {
    let auth = authorized(&state, &headers).await?;
    Ok(Json(DataResponse {
        data: state
            .identity()
            .add_passkey_options(&auth, &input.name)
            .await
            .map_err(problem)?,
    }))
}
#[utoipa::path(post, path = "/api/v1/me/passkeys/complete", request_body = CeremonyCompleteRequest, responses((status = 200, body = DataResponse<crate::modules::identity::interface::PasskeyView>), (status = 401, body = Problem)))]
pub async fn passkey_complete(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(input): Json<CeremonyCompleteRequest>,
) -> Result<Json<DataResponse<crate::modules::identity::interface::PasskeyView>>, Problem> {
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
    let cookie = format!(
        "janus_recovery={}; Path=/api/v1/auth/recovery; Max-Age=600; Secure; HttpOnly; SameSite=Strict",
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
#[utoipa::path(post, path = "/api/v1/auth/recovery/passkey/options", request_body = PasskeyOptionsRequest, responses((status = 200, body = DataResponse<crate::modules::identity::interface::CeremonyOptions>), (status = 400, body = Problem)))]
pub async fn recovery_passkey_options(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(input): Json<PasskeyOptionsRequest>,
) -> Result<Json<DataResponse<crate::modules::identity::interface::CeremonyOptions>>, Problem> {
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
#[utoipa::path(post, path = "/api/v1/auth/recovery/passkey/complete", request_body = CeremonyCompleteRequest, responses((status = 200, body = DataResponse<crate::modules::identity::interface::OwnerView>), (status = 400, body = Problem)))]
pub async fn recovery_passkey_complete(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(input): Json<CeremonyCompleteRequest>,
) -> Result<
    (
        HeaderMap,
        Json<DataResponse<crate::modules::identity::interface::OwnerView>>,
    ),
    Problem,
> {
    let token = cookie(&headers, "janus_recovery")
        .ok_or_else(|| problem(IdentityError::InvalidRecoveryCode))?;
    grant_response(
        state
            .identity()
            .recovery_passkey_complete(&token, &input.ceremony_id, input.credential)
            .await
            .map_err(problem)?,
    )
}

pub async fn authenticate(state: &AppState, headers: &HeaderMap) -> Result<AuthContext, Problem> {
    let token = cookie(headers, "__Host-janus_session");
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

#[allow(clippy::result_large_err)]
fn grant_response(
    grant: AuthenticationGrant,
) -> Result<
    (
        HeaderMap,
        Json<DataResponse<crate::modules::identity::interface::OwnerView>>,
    ),
    Problem,
> {
    let mut headers = HeaderMap::new();
    let cookie = format!(
        "__Host-janus_session={}; Path=/; Max-Age=604800; Secure; HttpOnly; SameSite=Strict",
        grant.session_token
    );
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
        IdentityError::Storage(_) | IdentityError::Data(_) | IdentityError::Internal(_) => {
            Problem::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "INTERNAL_ERROR",
                "Internal server error",
                "The identity operation could not be completed.",
            )
        }
    }
}

#[allow(dead_code)]
fn _request_context(_: Extension<RequestContext>) {}
