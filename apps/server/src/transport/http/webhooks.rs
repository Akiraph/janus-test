//! HTTP adapter for the optional email/webhook PR automation protocol.
//!
//! The endpoint accepts the fixed `fork_sync_conflict` JSON contract emitted by
//! the fork-sync job. Each conflict is retained as a separate repository item.

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
    application::automation::{AutomationError, ForkSyncAutomationItem, ForkSyncAutomationRequest},
    transport::http::{
        conditions::{RawBody, idempotency_request},
        dto::DataResponse,
        problem::{Problem, codes},
    },
};

#[derive(Debug, Clone)]
struct ParsedWebhook {
    workflow: String,
    source: String,
    github_credential_id: Option<String>,
    items: Vec<ParsedWebhookItem>,
}

#[derive(Debug, Clone)]
struct ParsedWebhookItem {
    pull_request_url: String,
    repository_url: String,
    parent_repository_url: Option<String>,
    default_branch: Option<String>,
    parent_default_branch: Option<String>,
    message: Option<String>,
    branch: Option<String>,
    project_name: String,
    github_credential_id: Option<String>,
}

#[utoipa::path(
    post,
    path = "/api/v1/automation/webhook",
    request_body(content = String, content_type = "application/json"),
    responses(
        (status = 202, body = DataResponse<OperationView>),
        (status = 401, body = Problem),
        (status = 415, body = Problem),
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
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .map(str::trim)
        .unwrap_or_default();
    if !content_type.eq_ignore_ascii_case("application/json") {
        return Err(Problem::new(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            codes::VALIDATION_FAILED,
            "Unsupported webhook media type",
            "fork_sync_conflict webhook payloads must use Content-Type application/json.",
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
        .request_fork_sync_automation(ForkSyncAutomationRequest {
            owner_id: owner_id.clone(),
            workflow: parsed.workflow.clone(),
            source: parsed.source.clone(),
            items: parsed
                .items
                .iter()
                .map(|item| ForkSyncAutomationItem {
                    pull_request_url: item.pull_request_url.clone(),
                    repository_url: item.repository_url.clone(),
                    parent_repository_url: item.parent_repository_url.clone(),
                    default_branch: item.default_branch.clone(),
                    parent_default_branch: item.parent_default_branch.clone(),
                    message: item.message.clone(),
                    branch: item.branch.clone(),
                    project_name: item.project_name.clone(),
                    github_credential_id: item.github_credential_id.clone(),
                })
                .collect(),
            github_credential_id: parsed.github_credential_id.clone(),
            github_token: state.config().automation_github_token.clone(),
            actor: serde_json::json!({
                "kind": "automation",
                "source": parsed.source,
                "workflow": parsed.workflow,
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
    let root = serde_json::from_slice::<Value>(body)
        .map_err(|error| format!("webhook body must be JSON: {error}"))?;
    let object = root
        .as_object()
        .ok_or_else(|| "webhook body must be a JSON object".to_owned())?;
    if object.get("event").and_then(Value::as_str) != Some("fork_sync_conflict") {
        return Err("unsupported webhook event; expected fork_sync_conflict".into());
    }
    if object
        .get("timestamp")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .is_none()
    {
        return Err("fork_sync_conflict timestamp is required".into());
    }
    if !object.get("summary").is_some_and(Value::is_object) {
        return Err("fork_sync_conflict summary is required".into());
    }
    let conflicts = object
        .get("conflicts")
        .and_then(Value::as_array)
        .ok_or_else(|| "fork_sync_conflict conflicts must be an array".to_owned())?;
    if conflicts.is_empty() {
        return Err("fork_sync_conflict conflicts must not be empty".into());
    }
    let github_credential_id = object
        .get("github_credential_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    let items = conflicts
        .iter()
        .enumerate()
        .map(|(index, value)| parse_conflict_item(value, index))
        .collect::<Result<Vec<_>, _>>()?;
    if items.is_empty() {
        return Err("payload contains no repository items".into());
    }
    Ok(ParsedWebhook {
        workflow: "fork-sync".into(),
        source: "fork_sync_conflict".into(),
        github_credential_id,
        items,
    })
}

fn parse_conflict_item(value: &Value, index: usize) -> Result<ParsedWebhookItem, String> {
    let full_name = value
        .get("fullName")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| value.split('/').count() == 2 && !value.contains(char::is_whitespace))
        .ok_or_else(|| format!("conflicts[{index}].fullName is invalid"))?;
    let repository_url = value
        .get("htmlUrl")
        .and_then(Value::as_str)
        .and_then(canonical_repository_url)
        .or_else(|| canonical_full_name(full_name))
        .ok_or_else(|| format!("conflicts[{index}].htmlUrl/fullName is invalid"))?;
    let pull_request_url = value
        .get("prUrl")
        .and_then(Value::as_str)
        .and_then(|value| canonical_pull_request(value).map(|item| item.0))
        .ok_or_else(|| format!("conflicts[{index}].prUrl is invalid"))?;
    let parent_full_name = value
        .get("parentFullName")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| value.split('/').count() == 2 && !value.contains(char::is_whitespace));
    let parent_repository_url = parent_full_name.and_then(canonical_full_name);
    let default_branch = required_branch(value, "defaultBranch", index)?;
    let parent_default_branch = required_branch(value, "parentDefaultBranch", index)?;
    let message = value
        .get("message")
        .and_then(Value::as_str)
        .map(normalize_name)
        .filter(|value| !value.is_empty());
    Ok(ParsedWebhookItem {
        pull_request_url,
        repository_url,
        parent_repository_url,
        default_branch: Some(default_branch.clone()),
        parent_default_branch: Some(parent_default_branch),
        message,
        branch: Some(default_branch),
        project_name: full_name.to_owned(),
        github_credential_id: value
            .get("github_credential_id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned),
    })
}

fn required_branch(value: &Value, field: &str, index: usize) -> Result<String, String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .and_then(normalize_branch)
        .ok_or_else(|| format!("conflicts[{index}].{field} is invalid"))
}

fn canonical_full_name(full_name: &str) -> Option<String> {
    let mut segments = full_name.split('/');
    let owner = segments.next()?.trim();
    let repository = segments.next()?.trim();
    if owner.is_empty() || repository.is_empty() || segments.next().is_some() {
        return None;
    }
    Some(format!("https://github.com/{owner}/{repository}.git"))
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

fn canonical_repository_url(candidate: &str) -> Option<String> {
    let parsed = Url::parse(candidate.trim()).ok()?;
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
    if segments.len() < 2 {
        return None;
    }
    let owner = segments[0].trim();
    let repository = segments[1].trim_end_matches(".git").trim();
    if owner.is_empty() || repository.is_empty() {
        return None;
    }
    Some(format!("https://github.com/{owner}/{repository}.git"))
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
    let items = parsed
        .items
        .iter()
        .map(|item| {
            format!(
                "{}\0{}\0{}",
                item.pull_request_url,
                item.repository_url,
                item.branch.as_deref().unwrap_or_default()
            )
        })
        .collect::<Vec<_>>()
        .join("\0");
    let normalized = format!("{}\0{}\0{}", parsed.workflow, parsed.source, items);
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
    fn parses_fixed_fork_sync_conflict_schema() {
        let input = br#"{
          "event":"fork_sync_conflict",
          "timestamp":"2026-08-18T06:00:00.000Z",
          "summary":{"scanned":8,"conflicts":2,"merged":2,"prOpen":1,"upToDate":1,"errors":1},
          "conflicts":[
            {"fullName":"Chloemlla/nginx-config","htmlUrl":"https://github.com/Chloemlla/nginx-config","parentFullName":"nginx/nginx","defaultBranch":"main","parentDefaultBranch":"master","prNumber":17,"prUrl":"https://github.com/Chloemlla/nginx-config/pull/17","message":"conflict"},
            {"fullName":"Chloemlla/react-starter","htmlUrl":"https://github.com/Chloemlla/react-starter","parentFullName":"facebook/react","defaultBranch":"main","parentDefaultBranch":"main","prNumber":42,"prUrl":"https://github.com/Chloemlla/react-starter/pull/42","message":"conflict"}
          ]
        }"#;
        let parsed = parse_webhook(input).expect("PR URL");
        assert_eq!(parsed.items.len(), 2);
        assert_eq!(
            parsed.items[0].pull_request_url,
            "https://github.com/Chloemlla/nginx-config/pull/17"
        );
        assert_eq!(
            parsed.items[0].repository_url,
            "https://github.com/Chloemlla/nginx-config.git"
        );
        assert_eq!(
            parsed.items[0].parent_default_branch.as_deref(),
            Some("master")
        );
        assert_eq!(parsed.items[1].project_name, "Chloemlla/react-starter");
    }

    #[test]
    fn rejects_empty_conflict_batches() {
        let input = br#"{"event":"fork_sync_conflict","timestamp":"2026-08-18T06:00:00.000Z","summary":{"scanned":1},"conflicts":[]}"#;
        assert!(parse_webhook(input).is_err());
    }

    #[test]
    fn rejects_other_events() {
        let input = br#"{"event":"fork_sync_ok","timestamp":"2026-08-18T06:00:00.000Z","summary":{},"conflicts":[]}"#;
        assert!(parse_webhook(input).is_err());
    }

    #[test]
    fn derived_idempotency_includes_the_branch() {
        let headers = HeaderMap::new();
        let input = br#"{"event":"fork_sync_conflict","timestamp":"2026-08-18T06:00:00.000Z","summary":{},"conflicts":[{"fullName":"acme/widget","htmlUrl":"https://github.com/acme/widget","parentFullName":"acme/upstream","defaultBranch":"main","parentDefaultBranch":"main","prNumber":42,"prUrl":"https://github.com/acme/widget/pull/42","message":"conflict"}]}"#;
        let mut first = parse_webhook(input).expect("PR URL");
        let first_key = webhook_idempotency(&headers, "owner", &first, b"html").key;
        first.items[0].branch = Some("repair/address".into());
        let second_key = webhook_idempotency(&headers, "owner", &first, b"html").key;
        assert_ne!(first_key, second_key);
    }
}
