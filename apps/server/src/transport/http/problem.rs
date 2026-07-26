use axum::{
    Json,
    http::{HeaderValue, StatusCode, header::CONTENT_TYPE},
    response::{IntoResponse, Response},
};
use serde::Serialize;
use utoipa::ToSchema;

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct Problem {
    #[serde(rename = "type")]
    pub type_url: String,
    pub title: String,
    pub status: u16,
    pub code: String,
    pub detail: String,
    pub request_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_cursor: Option<String>,
}

impl Problem {
    pub fn new(status: StatusCode, code: &str, title: &str, detail: impl Into<String>) -> Self {
        Self {
            type_url: format!("https://janus.local/problems/{code}"),
            title: title.into(),
            status: status.as_u16(),
            code: code.into(),
            detail: detail.into(),
            request_id: None,
            current_cursor: None,
        }
    }

    pub fn with_cursor(mut self, cursor: u64) -> Self {
        self.current_cursor = Some(cursor.to_string());
        self
    }

    /// Build a Problem from a stable application code using the shared status map.
    pub fn from_code(code: &str, detail: impl Into<String>) -> Self {
        let (status, title) = code_status_title(code);
        Self::new(status, code, title, detail)
    }
}

/// Stable M3 (+ shared M0–M2) problem codes with recommended HTTP status + title.
/// Handlers may still call `Problem::new` directly when they need a custom title.
pub mod codes {
    // Shared / pre-M3
    pub const RESOURCE_NOT_FOUND: &str = "RESOURCE_NOT_FOUND";
    pub const RESOURCE_VERSION_MISMATCH: &str = "RESOURCE_VERSION_MISMATCH";
    pub const PRECONDITION_REQUIRED: &str = "PRECONDITION_REQUIRED";
    pub const IDEMPOTENCY_KEY_REUSED: &str = "IDEMPOTENCY_KEY_REUSED";
    pub const OPERATION_IN_PROGRESS: &str = "OPERATION_IN_PROGRESS";
    pub const VALIDATION_FAILED: &str = "VALIDATION_FAILED";
    pub const INTERNAL_ERROR: &str = "INTERNAL_ERROR";

    // M3 sessions / turn
    pub const SESSION_NOT_FOUND: &str = "SESSION_NOT_FOUND";
    pub const ACTIVE_TURN_EXISTS: &str = "ACTIVE_TURN_EXISTS";
    pub const SESSION_DELETING: &str = "SESSION_DELETING";
    pub const TIMELINE_CURSOR_INVALID: &str = "TIMELINE_CURSOR_INVALID";

    // M3 models
    pub const PROVIDER_STREAM_FAILED: &str = "PROVIDER_STREAM_FAILED";
    pub const PROVIDER_AUTH_FAILED: &str = "PROVIDER_AUTH_FAILED";
    pub const MODEL_NOT_CONFIGURED: &str = "MODEL_NOT_CONFIGURED";

    // M3 tools / media
    pub const TOOL_NOT_ALLOWED: &str = "TOOL_NOT_ALLOWED";
    pub const TOOL_PATH_INVALID: &str = "TOOL_PATH_INVALID";
    pub const IMAGE_TOO_LARGE: &str = "IMAGE_TOO_LARGE";
    pub const UNSUPPORTED_IMAGE: &str = "UNSUPPORTED_IMAGE";
}

fn code_status_title(code: &str) -> (StatusCode, &'static str) {
    use codes::*;
    match code {
        RESOURCE_NOT_FOUND | SESSION_NOT_FOUND => (StatusCode::NOT_FOUND, "Resource not found"),
        RESOURCE_VERSION_MISMATCH => (StatusCode::PRECONDITION_FAILED, "Resource version mismatch"),
        PRECONDITION_REQUIRED => (StatusCode::PRECONDITION_REQUIRED, "Precondition required"),
        IDEMPOTENCY_KEY_REUSED => (StatusCode::CONFLICT, "Idempotency key reused"),
        OPERATION_IN_PROGRESS | ACTIVE_TURN_EXISTS | SESSION_DELETING => {
            (StatusCode::CONFLICT, "Operation conflict")
        }
        VALIDATION_FAILED
        | TIMELINE_CURSOR_INVALID
        | TOOL_PATH_INVALID
        | IMAGE_TOO_LARGE
        | UNSUPPORTED_IMAGE
        | TOOL_NOT_ALLOWED => (StatusCode::UNPROCESSABLE_ENTITY, "Validation failed"),
        PROVIDER_AUTH_FAILED => (StatusCode::BAD_GATEWAY, "Provider authentication failed"),
        PROVIDER_STREAM_FAILED => (StatusCode::BAD_GATEWAY, "Provider stream failed"),
        MODEL_NOT_CONFIGURED => (StatusCode::UNPROCESSABLE_ENTITY, "Model not configured"),
        INTERNAL_ERROR => (StatusCode::INTERNAL_SERVER_ERROR, "Internal server error"),
        _ => (StatusCode::BAD_REQUEST, "Request failed"),
    }
}

impl IntoResponse for Problem {
    fn into_response(self) -> Response {
        let status = StatusCode::from_u16(self.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        let mut response = (status, Json(self)).into_response();
        response.headers_mut().insert(
            CONTENT_TYPE,
            HeaderValue::from_static("application/problem+json"),
        );
        response
    }
}

#[cfg(test)]
mod tests {
    use super::{Problem, codes};
    use axum::http::StatusCode;

    #[test]
    fn m3_codes_map_to_stable_status() {
        let p = Problem::from_code(
            codes::ACTIVE_TURN_EXISTS,
            "session already has a running turn",
        );
        assert_eq!(p.status, StatusCode::CONFLICT.as_u16());
        assert_eq!(p.code, codes::ACTIVE_TURN_EXISTS);

        let p = Problem::from_code(codes::SESSION_NOT_FOUND, "missing");
        assert_eq!(p.status, StatusCode::NOT_FOUND.as_u16());

        let p = Problem::from_code(codes::IMAGE_TOO_LARGE, "too many pixels");
        assert_eq!(p.status, StatusCode::UNPROCESSABLE_ENTITY.as_u16());

        let p = Problem::from_code(codes::PROVIDER_STREAM_FAILED, "upstream closed");
        assert_eq!(p.status, StatusCode::BAD_GATEWAY.as_u16());
    }
}
