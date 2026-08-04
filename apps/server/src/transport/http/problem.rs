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
    pub const WORKSPACE_PROPAGATION_CONFLICT: &str = "WORKSPACE_PROPAGATION_CONFLICT";
    pub const TIMELINE_CURSOR_INVALID: &str = "TIMELINE_CURSOR_INVALID";
    pub const TURN_NOT_INTERACTIVE: &str = "TURN_NOT_INTERACTIVE";
    pub const TURN_TERMINAL: &str = "TURN_TERMINAL";
    pub const ASK_NOT_FOUND: &str = "ASK_NOT_FOUND";
    pub const ASK_NOT_OPEN: &str = "ASK_NOT_OPEN";

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
    pub const COMMAND_FORBIDDEN: &str = "COMMAND_FORBIDDEN";
    pub const NETWORK_POLICY_DENIED: &str = "NETWORK_POLICY_DENIED";
    pub const RUNTIME_UNAVAILABLE: &str = "RUNTIME_UNAVAILABLE";
    pub const JOB_LOST: &str = "JOB_LOST";
    pub const SERVICE_LOST: &str = "SERVICE_LOST";
    pub const TERMINAL_TICKET_INVALID: &str = "TERMINAL_TICKET_INVALID";
    pub const TERMINAL_SCROLLBACK_EXPIRED: &str = "TERMINAL_SCROLLBACK_EXPIRED";
    pub const TERMINAL_NOT_WRITABLE: &str = "TERMINAL_NOT_WRITABLE";
}

fn code_status_title(code: &str) -> (StatusCode, &'static str) {
    use codes::*;
    match code {
        RESOURCE_NOT_FOUND | SESSION_NOT_FOUND | ASK_NOT_FOUND => {
            (StatusCode::NOT_FOUND, "Resource not found")
        }
        RESOURCE_VERSION_MISMATCH => (StatusCode::PRECONDITION_FAILED, "Resource version mismatch"),
        PRECONDITION_REQUIRED => (StatusCode::PRECONDITION_REQUIRED, "Precondition required"),
        IDEMPOTENCY_KEY_REUSED => (StatusCode::CONFLICT, "Idempotency key reused"),
        OPERATION_IN_PROGRESS
        | ACTIVE_TURN_EXISTS
        | SESSION_DELETING
        | WORKSPACE_PROPAGATION_CONFLICT
        | RESOURCE_BUSY
        | TURN_NOT_INTERACTIVE
        | TURN_TERMINAL
        | ASK_NOT_OPEN => (StatusCode::CONFLICT, "Operation conflict"),
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
        MODEL_UNAVAILABLE | RUNTIME_UNAVAILABLE | JOB_LOST | SERVICE_LOST => (
            StatusCode::SERVICE_UNAVAILABLE,
            "Runtime dependency unavailable",
        ),
        RATE_LIMITED => (StatusCode::TOO_MANY_REQUESTS, "Rate limited"),
        COMMAND_FORBIDDEN | NETWORK_POLICY_DENIED => {
            (StatusCode::FORBIDDEN, "Runtime policy denied")
        }
        TERMINAL_TICKET_INVALID => (StatusCode::UNAUTHORIZED, "Terminal ticket invalid"),
        TERMINAL_SCROLLBACK_EXPIRED => (StatusCode::GONE, "Terminal scrollback expired"),
        TERMINAL_NOT_WRITABLE => (StatusCode::CONFLICT, "Terminal not writable"),
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
        janus_runtime::interface::RuntimeErrorCode::CommandForbidden => {
            Problem::from_code(COMMAND_FORBIDDEN, error.to_string())
        }
        janus_runtime::interface::RuntimeErrorCode::NetworkPolicyDenied => {
            Problem::from_code(NETWORK_POLICY_DENIED, error.to_string())
        }
        janus_runtime::interface::RuntimeErrorCode::RuntimeUnavailable => {
            Problem::from_code(RUNTIME_UNAVAILABLE, error.to_string())
        }
        janus_runtime::interface::RuntimeErrorCode::JobLost => {
            Problem::from_code(JOB_LOST, error.to_string())
        }
        janus_runtime::interface::RuntimeErrorCode::ServiceLost => {
            Problem::from_code(SERVICE_LOST, error.to_string())
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

#[cfg(test)]
mod tests {
    use super::{Problem, codes};
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
}
