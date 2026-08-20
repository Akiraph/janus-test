use axum::{
    Json,
    http::{HeaderValue, StatusCode, header::CONTENT_TYPE},
    response::{IntoResponse, Response},
};
use serde::Serialize;
use utoipa::ToSchema;

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct Problem {
    // Problems cross nearly every HTTP handler as the Result error. Boxed
    // strings keep that common error path compact without changing JSON or
    // OpenAPI representation, which remains a normal string in the response.
    #[serde(rename = "type")]
    pub type_url: Box<str>,
    pub title: Box<str>,
    pub status: u16,
    pub code: Box<str>,
    pub detail: Box<str>,
    pub request_id: Option<Box<str>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_cursor: Option<Box<str>>,
}

impl Problem {
    pub fn new(status: StatusCode, code: &str, title: &str, detail: impl Into<String>) -> Self {
        Self {
            type_url: format!("https://janus.local/problems/{code}").into_boxed_str(),
            title: title.to_owned().into_boxed_str(),
            status: status.as_u16(),
            code: code.to_owned().into_boxed_str(),
            detail: detail.into().into_boxed_str(),
            request_id: None,
            current_cursor: None,
        }
    }

    pub fn with_cursor(mut self, cursor: u64) -> Self {
        self.current_cursor = Some(cursor.to_string().into_boxed_str());
        self
    }

    /// Build a Problem from a stable application code using the shared status map.
    pub fn from_code(code: &str, detail: impl Into<String>) -> Self {
        let (status, title) = code_status_title(code);
        Self::new(status, code, title, detail)
    }
}

/// Stable public problem codes with recommended HTTP status and title.
/// Handlers may still call `Problem::new` directly when they need a custom title.
pub mod codes {
    // Shared
    pub const RESOURCE_NOT_FOUND: &str = "RESOURCE_NOT_FOUND";
    pub const RESOURCE_VERSION_MISMATCH: &str = "RESOURCE_VERSION_MISMATCH";
    pub const PRECONDITION_REQUIRED: &str = "PRECONDITION_REQUIRED";
    pub const IDEMPOTENCY_KEY_REUSED: &str = "IDEMPOTENCY_KEY_REUSED";
    pub const OPERATION_IN_PROGRESS: &str = "OPERATION_IN_PROGRESS";
    pub const VALIDATION_FAILED: &str = "VALIDATION_FAILED";
    pub const INTERNAL_ERROR: &str = "INTERNAL_ERROR";

    // Sessions / Turn
    pub const SESSION_NOT_FOUND: &str = "SESSION_NOT_FOUND";
    pub const ACTIVE_TURN_EXISTS: &str = "ACTIVE_TURN_EXISTS";
    pub const SESSION_DELETING: &str = "SESSION_DELETING";
    pub const TIMELINE_CURSOR_INVALID: &str = "TIMELINE_CURSOR_INVALID";
    pub const TURN_NOT_INTERACTIVE: &str = "TURN_NOT_INTERACTIVE";
    pub const TURN_TERMINAL: &str = "TURN_TERMINAL";

    // Models
    pub const PROVIDER_STREAM_FAILED: &str = "PROVIDER_STREAM_FAILED";
    pub const PROVIDER_AUTH_FAILED: &str = "PROVIDER_AUTH_FAILED";
    pub const MODEL_NOT_CONFIGURED: &str = "MODEL_NOT_CONFIGURED";

    // Tools / media
    pub const TOOL_NOT_ALLOWED: &str = "TOOL_NOT_ALLOWED";
    pub const TOOL_PATH_INVALID: &str = "TOOL_PATH_INVALID";
    pub const IMAGE_TOO_LARGE: &str = "IMAGE_TOO_LARGE";
    pub const UNSUPPORTED_IMAGE: &str = "UNSUPPORTED_IMAGE";

    // Runtime and model resilience
    pub const RESOURCE_BUSY: &str = "RESOURCE_BUSY";
    pub const MODEL_CONTEXT_EXCEEDED: &str = "MODEL_CONTEXT_EXCEEDED";
    pub const MODEL_CAPABILITY_MISMATCH: &str = "MODEL_CAPABILITY_MISMATCH";
    pub const MODEL_CONFIGURATION_FAULT: &str = "MODEL_CONFIGURATION_FAULT";
    pub const MODEL_UNAVAILABLE: &str = "MODEL_UNAVAILABLE";
    pub const RATE_LIMITED: &str = "RATE_LIMITED";
    pub const RUNTIME_UNAVAILABLE: &str = "RUNTIME_UNAVAILABLE";
    pub const ASYNC_TASK_LOST: &str = "ASYNC_TASK_LOST";
    pub const TERMINAL_TICKET_INVALID: &str = "TERMINAL_TICKET_INVALID";
    pub const TERMINAL_SCROLLBACK_EXPIRED: &str = "TERMINAL_SCROLLBACK_EXPIRED";
    pub const TERMINAL_NOT_WRITABLE: &str = "TERMINAL_NOT_WRITABLE";

    // Framework-level rejections wrapped by `client_error_envelope`: a request
    // that never reached a handler still answers with a code the client can
    // switch on instead of an untyped plain-text body.
    pub const METHOD_NOT_ALLOWED: &str = "METHOD_NOT_ALLOWED";
    pub const PAYLOAD_TOO_LARGE: &str = "PAYLOAD_TOO_LARGE";
    pub const UNSUPPORTED_MEDIA_TYPE: &str = "UNSUPPORTED_MEDIA_TYPE";
    pub const REQUEST_REJECTED: &str = "REQUEST_REJECTED";
}

fn code_status_title(code: &str) -> (StatusCode, &'static str) {
    use codes::*;
    match code {
        RESOURCE_NOT_FOUND | SESSION_NOT_FOUND => (StatusCode::NOT_FOUND, "Resource not found"),
        RESOURCE_VERSION_MISMATCH => (StatusCode::PRECONDITION_FAILED, "Resource version mismatch"),
        PRECONDITION_REQUIRED => (StatusCode::PRECONDITION_REQUIRED, "Precondition required"),
        IDEMPOTENCY_KEY_REUSED => (StatusCode::CONFLICT, "Idempotency key reused"),
        OPERATION_IN_PROGRESS
        | ACTIVE_TURN_EXISTS
        | SESSION_DELETING
        | RESOURCE_BUSY
        | TURN_NOT_INTERACTIVE
        | TURN_TERMINAL => (StatusCode::CONFLICT, "Operation conflict"),
        VALIDATION_FAILED
        | TIMELINE_CURSOR_INVALID
        | TOOL_PATH_INVALID
        | IMAGE_TOO_LARGE
        | UNSUPPORTED_IMAGE
        | TOOL_NOT_ALLOWED => (StatusCode::UNPROCESSABLE_ENTITY, "Validation failed"),
        PROVIDER_AUTH_FAILED => (StatusCode::BAD_GATEWAY, "Provider authentication failed"),
        PROVIDER_STREAM_FAILED => (StatusCode::BAD_GATEWAY, "Provider stream failed"),
        MODEL_NOT_CONFIGURED | MODEL_CONFIGURATION_FAULT => (
            StatusCode::UNPROCESSABLE_ENTITY,
            "Model configuration fault",
        ),
        MODEL_CONTEXT_EXCEEDED => (StatusCode::CONFLICT, "Model context exceeded"),
        MODEL_CAPABILITY_MISMATCH => (StatusCode::CONFLICT, "Model capability mismatch"),
        MODEL_UNAVAILABLE | RUNTIME_UNAVAILABLE | ASYNC_TASK_LOST => (
            StatusCode::SERVICE_UNAVAILABLE,
            "Runtime dependency unavailable",
        ),
        RATE_LIMITED => (StatusCode::TOO_MANY_REQUESTS, "Rate limited"),
        TERMINAL_TICKET_INVALID => (StatusCode::UNAUTHORIZED, "Terminal ticket invalid"),
        TERMINAL_SCROLLBACK_EXPIRED => (StatusCode::GONE, "Terminal scrollback expired"),
        TERMINAL_NOT_WRITABLE => (StatusCode::CONFLICT, "Terminal not writable"),
        METHOD_NOT_ALLOWED => (StatusCode::METHOD_NOT_ALLOWED, "Method not allowed"),
        PAYLOAD_TOO_LARGE => (StatusCode::PAYLOAD_TOO_LARGE, "Payload too large"),
        UNSUPPORTED_MEDIA_TYPE => (StatusCode::UNSUPPORTED_MEDIA_TYPE, "Unsupported media type"),
        INTERNAL_ERROR => (StatusCode::INTERNAL_SERVER_ERROR, "Internal server error"),
        _ => (StatusCode::BAD_REQUEST, "Request failed"),
    }
}

/// Map a `RuntimeError` to a stable public `Problem`. Runtime error codes are
/// the single source of truth for HTTP status and problem code.
pub fn map_runtime_error(error: janus_runtime::interface::RuntimeError) -> Problem {
    use codes::*;
    let code = error.code();
    match code {
        janus_runtime::interface::RuntimeErrorCode::ValidationFailed => {
            Problem::from_code(VALIDATION_FAILED, error.to_string())
        }
        janus_runtime::interface::RuntimeErrorCode::ResourceBusy => {
            Problem::from_code(RESOURCE_BUSY, error.to_string())
        }
        janus_runtime::interface::RuntimeErrorCode::RuntimeUnavailable => {
            Problem::from_code(RUNTIME_UNAVAILABLE, error.to_string())
        }
        janus_runtime::interface::RuntimeErrorCode::AsyncTaskLost => {
            Problem::from_code(ASYNC_TASK_LOST, error.to_string())
        }
        janus_runtime::interface::RuntimeErrorCode::TerminalTicketInvalid => {
            Problem::from_code(TERMINAL_TICKET_INVALID, error.to_string())
        }
        janus_runtime::interface::RuntimeErrorCode::TerminalScrollbackExpired => {
            Problem::from_code(TERMINAL_SCROLLBACK_EXPIRED, error.to_string())
        }
        janus_runtime::interface::RuntimeErrorCode::TerminalNotWritable => {
            Problem::from_code(TERMINAL_NOT_WRITABLE, error.to_string())
        }
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

/// Cap on how much of a non-Problem error body is read back for `detail`.
const REJECTION_BODY_LIMIT: usize = 8 * 1024;

/// Wrap client errors that no handler produced — extractor rejections, unmatched
/// routes, method mismatches — in the same `application/problem+json` envelope
/// handlers return.
///
/// Without this a malformed body or a missing query parameter reaches the web
/// client as an untyped plain-text 4xx, which it can only render as "Janus
/// returned 422" because there is no `code` to switch on. The framework's own
/// rejection text is kept as `detail`: it names the field or parameter at fault,
/// which is the part the client has to show.
pub async fn client_error_envelope(response: Response, request_id: &str) -> Response {
    if !response.status().is_client_error() || is_problem(&response) {
        return response;
    }
    let (parts, body) = response.into_parts();
    let (code, title) = rejection_code_title(parts.status);
    let body_text = match axum::body::to_bytes(body, REJECTION_BODY_LIMIT).await {
        Ok(bytes) => rejection_detail(&bytes),
        Err(_) => None,
    };
    let detail = body_text.unwrap_or_else(|| title.to_owned());
    let mut problem = Problem::new(parts.status, code, title, detail);
    problem.request_id = Some(request_id.to_owned().into_boxed_str());
    let mut response = problem.into_response();
    // A 405 must keep the `Allow` the router already computed.
    if let Some(allow) = parts.headers.get(axum::http::header::ALLOW) {
        response
            .headers_mut()
            .insert(axum::http::header::ALLOW, allow.clone());
    }
    response
}

fn is_problem(response: &Response) -> bool {
    response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.starts_with("application/problem+json"))
}

fn rejection_detail(bytes: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(bytes).ok()?.trim();
    if text.is_empty() {
        return None;
    }
    Some(text.to_owned())
}

fn rejection_code_title(status: StatusCode) -> (&'static str, &'static str) {
    use codes::*;
    match status {
        StatusCode::BAD_REQUEST | StatusCode::UNPROCESSABLE_ENTITY => {
            (VALIDATION_FAILED, "Validation failed")
        }
        StatusCode::NOT_FOUND => (RESOURCE_NOT_FOUND, "Resource not found"),
        StatusCode::METHOD_NOT_ALLOWED => (METHOD_NOT_ALLOWED, "Method not allowed"),
        StatusCode::PAYLOAD_TOO_LARGE => (PAYLOAD_TOO_LARGE, "Payload too large"),
        StatusCode::UNSUPPORTED_MEDIA_TYPE => (UNSUPPORTED_MEDIA_TYPE, "Unsupported media type"),
        _ => (REQUEST_REJECTED, "Request failed"),
    }
}

#[cfg(test)]
mod tests {
    use super::{Problem, codes, rejection_code_title};
    use axum::http::StatusCode;

    #[test]
    fn public_codes_map_to_stable_status() {
        let p = Problem::from_code(
            codes::ACTIVE_TURN_EXISTS,
            "session already has a running turn",
        );
        assert_eq!(p.status, StatusCode::CONFLICT.as_u16());
        assert_eq!(&*p.code, codes::ACTIVE_TURN_EXISTS);

        let p = Problem::from_code(codes::SESSION_NOT_FOUND, "missing");
        assert_eq!(p.status, StatusCode::NOT_FOUND.as_u16());

        let p = Problem::from_code(codes::IMAGE_TOO_LARGE, "too many pixels");
        assert_eq!(p.status, StatusCode::UNPROCESSABLE_ENTITY.as_u16());

        let p = Problem::from_code(codes::PROVIDER_STREAM_FAILED, "upstream closed");
        assert_eq!(p.status, StatusCode::BAD_GATEWAY.as_u16());

        let p = Problem::from_code(codes::RUNTIME_UNAVAILABLE, "probe failed");
        assert_eq!(p.status, StatusCode::SERVICE_UNAVAILABLE.as_u16());

        let p = Problem::from_code(codes::RATE_LIMITED, "retry later");
        assert_eq!(p.status, StatusCode::TOO_MANY_REQUESTS.as_u16());

        let p = Problem::from_code(codes::TERMINAL_SCROLLBACK_EXPIRED, "range expired");
        assert_eq!(p.status, StatusCode::GONE.as_u16());
    }

    #[test]
    fn framework_rejections_carry_a_switchable_code() {
        let (code, _) = rejection_code_title(StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(code, codes::VALIDATION_FAILED);

        let (code, _) = rejection_code_title(StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(code, codes::METHOD_NOT_ALLOWED);
        assert_eq!(
            Problem::from_code(code, "wrong method").status,
            StatusCode::METHOD_NOT_ALLOWED.as_u16()
        );

        let (code, _) = rejection_code_title(StatusCode::UNSUPPORTED_MEDIA_TYPE);
        assert_eq!(code, codes::UNSUPPORTED_MEDIA_TYPE);

        let (code, _) = rejection_code_title(StatusCode::IM_A_TEAPOT);
        assert_eq!(code, codes::REQUEST_REJECTED);
    }
}
