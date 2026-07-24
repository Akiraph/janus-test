//! Transport helpers for `Idempotency-Key` and `If-Match` preconditions.
//!
//! `API-IDEM-01` requires every POST/DELETE that creates work, has an external
//! side effect or is not safely repeatable to send an `Idempotency-Key`. The
//! key is scoped by owner + HTTP method + normalized route and is paired with a
//! request digest so a replay with a different body returns
//! `409 IDEMPOTENCY_KEY_REUSED` (handled inside `OperationInterface::create`).
//!
//! `API-COND-01` requires mutating single-resource requests to send `If-Match`
//! with the resource's opaque `version`. A missing header yields
//! `428 PRECONDITION_REQUIRED`; the actual mismatch (`412
//! RESOURCE_VERSION_MISMATCH`) is surfaced by the Module when its optimistic
//! update affects zero rows.

use axum::{
    body::{Bytes, to_bytes},
    extract::FromRequest,
    http::{HeaderMap, Request, StatusCode},
    response::{IntoResponse, Response},
};
use chrono::{Duration, Utc};
use sha2::{Digest, Sha256};

use crate::{
    platform::{clock::format_utc, operations::IdempotencyRequest},
    transport::http::problem::Problem,
};

/// Header name for the client-generated idempotency key.
pub const IDEMPOTENCY_KEY: &str = "idempotency-key";
/// Header name for the optimistic-concurrency ETag.
pub const IF_MATCH: &str = "if-match";
/// Idempotency record retention window.
const IDEMPOTENCY_TTL_SECONDS: i64 = 24 * 60 * 60;
/// Cap raw request bodies handled via `RawBody` (matches the public bootstrap
/// limit for attachments). Anything larger is rejected as payload-too-large at
/// the extractor boundary so handlers do not buffer unbounded data.
const RAW_BODY_LIMIT: usize = 25 * 1024 * 1024;

/// Raw request body extractor with a utoipa-opaque name.
///
/// `axum::body::Bytes` is recognized by `utoipa-gen` as a request-body argument
/// and forces it to derive a `ToSchema` for `Bytes`, which fails. Wrapping the
/// bytes in a newtype (`RawBody`) makes the macro treat the argument as a
/// generic extractor it does not introspect, while still letting the handler
/// read the raw bytes for idempotency digest computation.
#[derive(Debug, Clone, Default)]
pub struct RawBody(pub Bytes);

impl RawBody {
    pub fn as_slice(&self) -> &[u8] {
        self.0.as_ref()
    }
}

impl<S> FromRequest<S> for RawBody
where
    S: Send + Sync,
    Bytes: FromRequest<S>,
    Response: axum::response::IntoResponse + Send,
{
    type Rejection = Response;

    async fn from_request(
        req: Request<axum::body::Body>,
        _state: &S,
    ) -> Result<Self, Self::Rejection> {
        let bytes = to_bytes(req.into_body(), RAW_BODY_LIMIT)
            .await
            .map_err(|error| {
                let problem = Problem::new(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "PAYLOAD_TOO_LARGE",
                    "Payload too large",
                    error.to_string(),
                );
                problem.into_response()
            })?;
        Ok(Self(bytes))
    }
}

/// Build an `IdempotencyRequest` from the incoming headers and request bytes.
///
/// Returns `Ok(None)` when the caller did not send an `Idempotency-Key` header;
/// handlers that require idempotency should turn that into a `428`/`400`
/// Problem themselves. The digest is the lowercase hex SHA-256 of the raw
/// request body so retries with an identical body collapse to the stored
/// operation and a different body surfaces `IDEMPOTENCY_KEY_REUSED` inside
/// `OperationInterface::create`.
pub fn idempotency_request(
    headers: &HeaderMap,
    owner_id: &str,
    method: &str,
    normalized_route: &str,
    body: &[u8],
) -> Option<IdempotencyRequest> {
    let key = headers
        .get(IDEMPOTENCY_KEY)
        .and_then(|value| value.to_str().ok())?
        .trim()
        .to_owned();
    if key.is_empty() {
        return None;
    }
    let mut hasher = Sha256::new();
    hasher.update(body);
    let digest = hex::encode(hasher.finalize());
    let expires_at = format_utc(Utc::now() + Duration::seconds(IDEMPOTENCY_TTL_SECONDS));
    Some(IdempotencyRequest {
        key,
        owner_id: owner_id.to_owned(),
        method: method.to_owned(),
        normalized_route: normalized_route.to_owned(),
        digest,
        expires_at,
    })
}

/// Require an `Idempotency-Key` header. Returns the parsed `IdempotencyRequest`
/// on success, or a `422` Problem explaining that the key is required for the
/// command per `API-IDEM-01`.
#[allow(clippy::result_large_err)]
pub fn require_idempotency(
    headers: &HeaderMap,
    owner_id: &str,
    method: &str,
    normalized_route: &str,
    body: &[u8],
) -> Result<IdempotencyRequest, Problem> {
    idempotency_request(headers, owner_id, method, normalized_route, body).ok_or_else(|| {
        Problem::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "VALIDATION_FAILED",
            "Idempotency-Key required",
            "Send a client-generated Idempotency-Key header for this mutating command.",
        )
    })
}

/// Extract the opaque version from an `If-Match` header.
///
/// The header carries a strong ETag like `"v_01J..."`; the surrounding quotes
/// are stripped. An empty or missing header yields a `428
/// PRECONDITION_REQUIRED` Problem so the client re-reads the resource and
/// retries with its current `version`.
#[allow(clippy::result_large_err)]
pub fn if_match_version(headers: &HeaderMap) -> Result<String, Problem> {
    let raw = headers
        .get(IF_MATCH)
        .and_then(|value| value.to_str().ok())
        .map(str::trim);
    let version = match raw {
        Some(value) => value,
        None => {
            return Err(Problem::new(
                StatusCode::PRECONDITION_REQUIRED,
                "PRECONDITION_REQUIRED",
                "If-Match required",
                "Send the resource version via the If-Match header.",
            ));
        }
    };
    let unquoted = version.trim_matches('"');
    if unquoted.is_empty() {
        return Err(Problem::new(
            StatusCode::PRECONDITION_REQUIRED,
            "PRECONDITION_REQUIRED",
            "If-Match required",
            "The If-Match header must carry the resource version.",
        ));
    }
    Ok(unquoted.to_owned())
}
