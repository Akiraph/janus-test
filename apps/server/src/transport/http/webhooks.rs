//! HTTP adapter for the optional email/webhook PR automation protocol.
//!
//! The endpoint accepts either the final HTML email body or a small JSON
//! envelope containing that body. Only a canonical GitHub pull-request URL is
//! extracted; the remaining email text is deliberately ignored.

use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode, header},
};
use chrono::{Duration, Utc};
use janus_infrastructure::{
    clock::format_utc,
    id::CorrelationId,
    operations::{IdempotencyRequest, OperationError, OperationView},
};
use janus_projects::interface::ProjectsError;
use serde_json::Value;
use sha2::{Digest, Sha256};
use url::Url;

use crate::{
    AppState,
    application::automation::{AutomationError, PullRequestAutomationRequest},
    transport::http::{
        conditions::{RawBody, idempotency_request},
        dto::DataResponse,
        problem::{Problem, codes},
    },
};

#[derive(Debug, Clone)]
struct ParsedWebhook {
    pull_request_url: String,
    repository_url: String,
    branch: Option<String>,
    project_name: String,
    github_credential_id: Option<String>,
}

#[utoipa::path(
    post,
    path = "/api/v1/automation/webhook",
    request_body(content = String, content_type = "text/html"),
    responses(
        (status = 202, body = DataResponse<OperationView>),
        (status = 401, body = Problem),
        (status = 404, body = Problem),
        (status = 422, body = Problem),
        (status = 503, body = Problem)
    )
)]
pub async fn webhook(
    State(state): State<AppState>,
    headers: HeaderMap,
    raw_body: RawBody,
) -> Result<(StatusCode, Json<DataResponse<OperationView>>), Problem> {
    if !state.config().automation_webhook_enabled {
        return Err(Problem::new(
            StatusCode::NOT_FOUND,
            codes::RESOURCE_NOT_FOUND,
            "Resource not found",
            "The requested automation endpoint is not enabled.",
        ));
    }
    let secret = state
        .config()
        .automation_webhook_secret
        .as_deref()
        .ok_or_else(|| {
            Problem::new(
                StatusCode::SERVICE_UNAVAILABLE,
                codes::INTERNAL_ERROR,
                "Automation unavailable",
                "The automation endpoint has no configured secret.",
            )
        })?;
    if !authorized(&headers, secret) {
        return Err(Problem::new(
            StatusCode::UNAUTHORIZED,
            "WEBHOOK_UNAUTHORIZED",
            "Webhook unauthorized",
            "The webhook secret is missing or invalid.",
        ));
    }

    let parsed = parse_webhook(raw_body.as_slice())
        .map_err(|detail| Problem::from_code(codes::VALIDATION_FAILED, detail))?;
    let owner_id = state.identity().single_owner_id().await.map_err(|error| {
        Problem::new(
            StatusCode::SERVICE_UNAVAILABLE,
            codes::INTERNAL_ERROR,
            "Owner unavailable",
            error.to_string(),
        )
    })?;
    let idempotency = webhook_idempotency(&headers, &owner_id, &parsed, raw_body.as_slice());
    let operation = state
        .application()
        .request_pull_request_automation(PullRequestAutomationRequest {
            owner_id: owner_id.clone(),
            pull_request_url: parsed.pull_request_url,
            repository_url: parsed.repository_url,
            branch: parsed.branch,
            project_name: parsed.project_name,
            github_credential_id: parsed.github_credential_id,
            github_token: state.config().automation_github_token.clone(),
            actor: serde_json::json!({
                "kind": "automation",
                "source": "github_webhook",
                "owner_id": owner_id,
            }),
            correlation_id: CorrelationId::new(),
            idempotency,
        })
        .await
        .map_err(automation_problem)?;
    Ok((StatusCode::ACCEPTED, Json(DataResponse { data: operation })))
}

fn authorized(headers: &HeaderMap, expected: &str) -> bool {
    let supplied = headers
        .get("x-janus-webhook-secret")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .or_else(|| {
            headers
                .get(header::AUTHORIZATION)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.strip_prefix("Bearer "))
                .map(str::trim)
        });
    let Some(supplied) = supplied else {
        return false;
    };
    constant_time_equal(expected.as_bytes(), supplied.as_bytes())
}

fn constant_time_equal(left: &[u8], right: &[u8]) -> bool {
    let mut difference = left.len() ^ right.len();
    for index in 0..left.len().max(right.len()) {
        difference |= usize::from(left.get(index).copied().unwrap_or_default())
            ^ usize::from(right.get(index).copied().unwrap_or_default());
    }
    difference == 0
}

fn parse_webhook(body: &[u8]) -> Result<ParsedWebhook, String> {
    let raw = String::from_utf8_lossy(body);
    let json = serde_json::from_slice::<Value>(body).ok();
    let mut text_fragments = vec![raw.into_owned()];
    if let Some(value) = &json {
        collect_strings(value, &mut text_fragments);
    }
    let source = decode_html_entities(&text_fragments.join("\n"));
    let (pull_request_url, owner, repository, number) = find_pull_request(&source)
        .ok_or_else(|| "payload does not contain a GitHub pull-request URL".to_owned())?;
    let branch = json
        .as_ref()
        .and_then(|value| {
            first_string_field(value, &["branch", "head_branch", "pull_request_branch"])
        })
        .and_then(|value| normalize_branch(&value));
    let project_name = json
        .as_ref()
        .and_then(|value| first_string_field(value, &["project_name", "project", "name"]))
        .map(|value| normalize_name(&value))
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| format!("{owner}/{repository} PR #{number}"));
    let github_credential_id = json
        .as_ref()
        .and_then(|value| first_string_field(value, &["github_credential_id"]))
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty());
    Ok(ParsedWebhook {
        pull_request_url,
        repository_url: format!("https://github.com/{owner}/{repository}.git"),
        branch,
        project_name,
        github_credential_id,
    })
}

fn collect_strings(value: &Value, output: &mut Vec<String>) {
    match value {
        Value::String(value) => output.push(value.clone()),
        Value::Array(values) => values
            .iter()
            .for_each(|value| collect_strings(value, output)),
        Value::Object(values) => values
            .values()
            .for_each(|value| collect_strings(value, output)),
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

fn first_string_field(value: &Value, names: &[&str]) -> Option<String> {
    let Value::Object(fields) = value else {
        return None;
    };
    names.iter().find_map(|name| {
        fields
            .get(*name)
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
    })
}

fn find_pull_request(source: &str) -> Option<(String, String, String, String)> {
    for prefix in [
        "https://github.com/",
        "http://github.com/",
        "https://www.github.com/",
        "http://www.github.com/",
    ] {
        let mut cursor = 0;
        while let Some(relative) = source[cursor..].find(prefix) {
            let start = cursor + relative;
            let end = source[start..]
                .char_indices()
                .find_map(|(offset, character)| {
                    (character.is_whitespace()
                        || ['"', '\'', '<', '>', ')', ']', '}', ',', ';'].contains(&character))
                    .then_some(start + offset)
                })
                .unwrap_or(source.len());
            let candidate = source[start..end].trim_end_matches(['.', ':']);
            if let Some(result) = canonical_pull_request(candidate) {
                return Some(result);
            }
            cursor = start + prefix.len();
            if cursor >= source.len() {
                break;
            }
        }
    }
    None
}

fn canonical_pull_request(candidate: &str) -> Option<(String, String, String, String)> {
    let parsed = Url::parse(candidate).ok()?;
    if !matches!(parsed.scheme(), "http" | "https")
        || !matches!(
            parsed.host_str(),
            Some("github.com") | Some("www.github.com")
        )
    {
        return None;
    }
    let segments = parsed
        .path_segments()?
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>();
    if segments.len() < 4
        || segments[2] != "pull"
        || !segments[3].bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let owner = segments[0].trim();
    let repository = segments[1].trim_end_matches(".git").trim();
    let number = segments[3].trim();
    if owner.is_empty() || repository.is_empty() || number.is_empty() {
        return None;
    }
    let pull_request_url = format!("https://github.com/{owner}/{repository}/pull/{number}");
    Some((
        pull_request_url,
        owner.to_owned(),
        repository.to_owned(),
        number.to_owned(),
    ))
}

fn decode_html_entities(value: &str) -> String {
    value
        .replace("&amp;", "&")
        .replace("&#x2F;", "/")
        .replace("&#47;", "/")
        .replace("&#x27;", "'")
        .replace("&#39;", "'")
        .replace("&quot;", "\"")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
}

fn normalize_branch(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty()
        || value.len() > 255
        || value.starts_with('-')
        || value
            .chars()
            .any(|character| character.is_whitespace() || character.is_control())
    {
        return None;
    }
    Some(value.to_owned())
}

fn normalize_name(value: &str) -> String {
    value
        .chars()
        .filter(|character| !character.is_control())
        .take(80)
        .collect::<String>()
        .trim()
        .to_owned()
}

fn webhook_idempotency(
    headers: &HeaderMap,
    owner_id: &str,
    parsed: &ParsedWebhook,
    body: &[u8],
) -> IdempotencyRequest {
    if let Some(request) = idempotency_request(
        headers,
        owner_id,
        "POST",
        "/api/v1/automation/webhook",
        body,
    ) {
        return request;
    }
    let normalized = format!(
        "{}\0{}",
        parsed.pull_request_url,
        parsed.branch.as_deref().unwrap_or_default()
    );
    let mut hasher = Sha256::new();
    hasher.update(normalized.as_bytes());
    let digest = hex::encode(hasher.finalize());
    IdempotencyRequest {
        key: format!("automation-pr-{digest}"),
        owner_id: owner_id.to_owned(),
        method: "POST".into(),
        normalized_route: "/api/v1/automation/webhook".into(),
        digest,
        expires_at: format_utc(Utc::now() + Duration::days(30)),
    }
}

fn automation_problem(error: AutomationError) -> Problem {
    match error {
        AutomationError::Validation(detail) => Problem::from_code(codes::VALIDATION_FAILED, detail),
        AutomationError::Projects(ProjectsError::Validation(detail)) => {
            Problem::from_code(codes::VALIDATION_FAILED, detail)
        }
        AutomationError::Projects(ProjectsError::NotFound | ProjectsError::CredentialNotFound) => {
            Problem::from_code(codes::RESOURCE_NOT_FOUND, error.to_string())
        }
        AutomationError::Operation(OperationError::Internal(error))
            if error.to_string().contains("IDEMPOTENCY_KEY_REUSED") =>
        {
            Problem::from_code(codes::IDEMPOTENCY_KEY_REUSED, error.to_string())
        }
        other => Problem::from_code(codes::INTERNAL_ERROR, other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use axum::http::HeaderMap;

    use super::{parse_webhook, webhook_idempotency};

    #[test]
    fn extracts_pr_from_final_report_html() {
        let input = br#"<table><tr><td><a href="https://github.com/acme/widget/pull/42">PR</a></td></tr></table>"#;
        let parsed = parse_webhook(input).expect("PR URL");
        assert_eq!(
            parsed.pull_request_url,
            "https://github.com/acme/widget/pull/42"
        );
        assert_eq!(parsed.repository_url, "https://github.com/acme/widget.git");
    }

    #[test]
    fn accepts_json_email_envelope_and_branch() {
        let input = br#"{"email_html":"<a href=\"https://github.com/acme/widget/pull/7\">conflict</a>","head_branch":"fix/address"}"#;
        let parsed = parse_webhook(input).expect("PR URL");
        assert_eq!(parsed.branch.as_deref(), Some("fix/address"));
    }

    #[test]
    fn ignores_non_pull_github_links() {
        assert!(parse_webhook(br#"<a href="https://github.com/acme/widget">repo</a>"#).is_err());
    }

    #[test]
    fn derived_idempotency_includes_the_branch() {
        let headers = HeaderMap::new();
        let mut first =
            parse_webhook(br#"<a href="https://github.com/acme/widget/pull/42">PR</a>"#)
                .expect("PR URL");
        let first_key = webhook_idempotency(&headers, "owner", &first, b"html").key;
        first.branch = Some("repair/address".into());
        let second_key = webhook_idempotency(&headers, "owner", &first, b"html").key;
        assert_ne!(first_key, second_key);
    }
}
