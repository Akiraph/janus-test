use std::{net::SocketAddr, path::PathBuf, time::Duration};

use janus_infrastructure::{
    id::{CorrelationId, ProjectId, SessionId, TurnId},
    operations::{
        CreateOperation, CreateWork, OperationCompletion, OperationStatus, StepState, WorkClaim,
        WorkFailureDisposition,
    },
};
use janus_server::{
    AppState,
    config::{Config, RunMode},
    router,
};
use reqwest::{Client, StatusCode};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::{net::TcpListener, task::JoinHandle};

const NOW: &str = "2026-07-31T00:00:00.000Z";

fn test_config(data_root: PathBuf) -> Config {
    Config {
        bind: SocketAddr::from(([127, 0, 0, 1], 0)),
        data_root,
        mode: RunMode::Development,
        development_auth: true,
        webauthn_rp_name: "Janus Test".into(),
        webauthn_rp_id: "localhost".into(),
        public_origin: url::Url::parse("http://localhost").expect("static test URL"),
        event_heartbeat: Duration::from_millis(50),
    }
}

struct LiveServer {
    base_url: String,
    task: JoinHandle<()>,
}

impl Drop for LiveServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn spawn(state: AppState) -> anyhow::Result<LiveServer> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let task = tokio::spawn(async move {
        if let Err(error) = axum::serve(listener, router(state)).await {
            panic!("test server failed: {error}");
        }
    });
    Ok(LiveServer {
        base_url: format!("http://{address}"),
        task,
    })
}

struct SeededSession {
    session_id: SessionId,
    active_turn_id: TurnId,
    queued_turn_id: Option<TurnId>,
}

async fn seed_session(
    state: &AppState,
    include_queued_turn: bool,
) -> anyhow::Result<SeededSession> {
    let pool = state.pool();
    let project_id = ProjectId::new();
    let session_id = SessionId::new();
    let active_turn_id = TurnId::new();
    let queued_turn_id = include_queued_turn.then(TurnId::new);

    sqlx::query(
        "INSERT INTO owners (id, display_name, created_at) \
         VALUES ('owner-cancel-test', 'Owner', ?)",
    )
    .bind(NOW)
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO projects \
         (id, owner_id, name, state, repo_access, repo_url, version, \
          created_at, updated_at, last_activity_at) \
         VALUES (?, 'owner-cancel-test', 'Project', 'ready', \
                 'public_https', 'https://example.com/repo.git', 'v_project', ?, ?, ?)",
    )
    .bind(project_id.to_string())
    .bind(NOW)
    .bind(NOW)
    .bind(NOW)
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO sessions \
         (id, project_id, kind, state, workspace_handle, active_turn_id, \
          source_main_revision_id, version, created_at, updated_at, last_activity_at) \
         VALUES (?, ?, 'regular', 'active', 'workspace', ?, 'revision', \
                 'v_session', ?, ?, ?)",
    )
    .bind(session_id.to_string())
    .bind(project_id.to_string())
    .bind(active_turn_id.to_string())
    .bind(NOW)
    .bind(NOW)
    .bind(NOW)
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO turns \
         (id, session_id, sequence, status, model_snapshot_json, version, created_at, updated_at) \
         VALUES (?, ?, 1, 'running', '{}', 'v_turn_1', ?, ?)",
    )
    .bind(active_turn_id.to_string())
    .bind(session_id.to_string())
    .bind(NOW)
    .bind(NOW)
    .execute(pool)
    .await?;
    if let Some(turn_id) = queued_turn_id {
        sqlx::query(
            "INSERT INTO turns \
             (id, session_id, sequence, status, model_snapshot_json, version, created_at, updated_at) \
             VALUES (?, ?, 2, 'queued', '{}', 'v_turn_2', ?, ?)",
        )
        .bind(turn_id.to_string())
        .bind(session_id.to_string())
        .bind(NOW)
        .bind(NOW)
        .execute(pool)
        .await?;
    }

    Ok(SeededSession {
        session_id,
        active_turn_id,
        queued_turn_id,
    })
}

#[tokio::test]
async fn restart_persists_wake_for_unstarted_turn() -> anyhow::Result<()> {
    let directory = TempDir::new()?;
    let active_turn_id;
    {
        let state = AppState::initialize(test_config(directory.path().into())).await?;
        active_turn_id = seed_session(&state, false).await?.active_turn_id;
    }

    let state = AppState::initialize(test_config(directory.path().into())).await?;
    let (status, active_session_turn): (String, Option<String>) = sqlx::query_as(
        "SELECT turns.status, sessions.active_turn_id \
         FROM turns JOIN sessions ON sessions.id = turns.session_id \
         WHERE turns.id = ?",
    )
    .bind(active_turn_id.to_string())
    .fetch_one(state.pool())
    .await?;
    assert_eq!(status, "running");
    assert_eq!(
        active_session_turn.as_deref(),
        Some(active_turn_id.to_string().as_str())
    );

    let (handler_kind, payload_json): (String, String) = sqlx::query_as(
        "SELECT handler_kind, payload_json FROM work_items \
         WHERE handler_kind = 'turn.execute' ORDER BY created_at DESC LIMIT 1",
    )
    .fetch_one(state.pool())
    .await?;
    assert_eq!(handler_kind, "turn.execute");
    let payload: Value = serde_json::from_str(&payload_json)?;
    assert_eq!(payload["turn_id"], active_turn_id.to_string());

    Ok(())
}

#[tokio::test]
async fn work_queue_bounds_attempts_and_dead_letters_failures() -> anyhow::Result<()> {
    let directory = TempDir::new()?;
    let state = AppState::initialize(test_config(directory.path().into())).await?;
    let now = janus_infrastructure::clock::now_utc_str();
    sqlx::query(
        "INSERT INTO work_items \
         (id, handler_kind, payload_json, not_before, lease_nonce, lease_expires_at, \
          attempts, dead, created_at) \
         VALUES ('bounded-work', 'test.failure', '{}', ?, NULL, NULL, 0, 0, ?)",
    )
    .bind(&now)
    .bind(&now)
    .execute(state.pool())
    .await?;

    for expected_attempt in 1..=5_i64 {
        let claimed = state
            .operations()
            .claim_work("test.failure", 60)
            .await?
            .expect("work item should remain claimable until the fifth attempt");
        let attempts: i64 =
            sqlx::query_scalar("SELECT attempts FROM work_items WHERE id = 'bounded-work'")
                .fetch_one(state.pool())
                .await?;
        assert_eq!(attempts, expected_attempt);
        assert!(
            state
                .operations()
                .fail_work(&claimed.id, &claimed.nonce, WorkFailureDisposition::Retry,)
                .await?
        );
        if expected_attempt < 5 {
            // Bypass the real delay so the test exercises the bound quickly.
            sqlx::query("UPDATE work_items SET not_before = ? WHERE id = 'bounded-work'")
                .bind(janus_infrastructure::clock::now_utc_str())
                .execute(state.pool())
                .await?;
        }
    }

    let (attempts, dead): (i64, i64) =
        sqlx::query_as("SELECT attempts, dead FROM work_items WHERE id = 'bounded-work'")
            .fetch_one(state.pool())
            .await?;
    assert_eq!(attempts, 5);
    assert_eq!(dead, 1);

    Ok(())
}

#[tokio::test]
async fn stale_operation_worker_cannot_publish_terminal_state() -> anyhow::Result<()> {
    let directory = TempDir::new()?;
    let state = AppState::initialize(test_config(directory.path().into())).await?;
    let created = state
        .operations()
        .create(
            CreateOperation {
                kind: "test.operation",
                actor: json!({"kind": "test"}),
                target_kind: "test_target",
                target_id: Some("target-1"),
                conditions: json!({}),
                correlation_id: CorrelationId::new(),
                idempotency: None,
            },
            Some(CreateWork {
                handler_kind: "test.operation",
                payload: json!({"target": "target-1"}),
            }),
        )
        .await?;
    let first_claim = state
        .operations()
        .claim_work("test.operation", 60)
        .await?
        .expect("first worker should claim the operation");
    assert!(matches!(
        state
            .operations()
            .begin_step_claimed(
                WorkClaim {
                    id: &first_claim.id,
                    nonce: &first_claim.nonce,
                },
                &created.operation.id,
                "external_effect",
                json!({"target": "target-1"}),
            )
            .await?,
        StepState::Running
    ));
    assert!(
        state
            .operations()
            .renew_work(&first_claim.id, &first_claim.nonce, 60)
            .await?
    );

    sqlx::query("UPDATE work_items SET lease_expires_at = '2000-01-01T00:00:00.000Z'")
        .execute(state.pool())
        .await?;
    assert!(
        !state
            .operations()
            .renew_work(&first_claim.id, &first_claim.nonce, 60)
            .await?
    );
    assert!(
        !state
            .operations()
            .finish_claimed(
                &created.operation.id,
                &first_claim.id,
                &first_claim.nonce,
                OperationCompletion {
                    status: OperationStatus::Succeeded,
                    result: Some(json!({"worker": "expired"})),
                    problem: None,
                    correlation_id: CorrelationId::new(),
                },
            )
            .await?
    );
    assert!(
        state
            .operations()
            .complete_step_claimed(
                WorkClaim {
                    id: &first_claim.id,
                    nonce: &first_claim.nonce,
                },
                &created.operation.id,
                "external_effect",
                None,
            )
            .await
            .is_err()
    );
    let second_claim = state
        .operations()
        .claim_work("test.operation", 60)
        .await?
        .expect("expired work should be reclaimable");
    assert_ne!(first_claim.nonce, second_claim.nonce);
    assert!(matches!(
        state
            .operations()
            .begin_step_claimed(
                WorkClaim {
                    id: &second_claim.id,
                    nonce: &second_claim.nonce,
                },
                &created.operation.id,
                "external_effect",
                json!({"target": "target-1"}),
            )
            .await?,
        StepState::NeedsReconciliation
    ));

    assert!(
        !state
            .operations()
            .finish_claimed(
                &created.operation.id,
                &second_claim.id,
                &first_claim.nonce,
                OperationCompletion {
                    status: OperationStatus::Succeeded,
                    result: Some(json!({"worker": "stale"})),
                    problem: None,
                    correlation_id: CorrelationId::new(),
                },
            )
            .await?
    );
    assert_eq!(
        state
            .operations()
            .get(&created.operation.id)
            .await?
            .expect("operation remains durable")
            .status,
        "running"
    );

    state
        .operations()
        .complete_step_claimed(
            WorkClaim {
                id: &second_claim.id,
                nonce: &second_claim.nonce,
            },
            &created.operation.id,
            "external_effect",
            None,
        )
        .await?;
    assert!(
        state
            .operations()
            .finish_claimed(
                &created.operation.id,
                &second_claim.id,
                &second_claim.nonce,
                OperationCompletion {
                    status: OperationStatus::Succeeded,
                    result: Some(json!({"worker": "current"})),
                    problem: None,
                    correlation_id: CorrelationId::new(),
                },
            )
            .await?
    );
    assert_eq!(
        state
            .operations()
            .get(&created.operation.id)
            .await?
            .expect("operation has terminal state")
            .status,
        "succeeded"
    );

    Ok(())
}

fn cancel_url(server: &LiveServer, session_id: SessionId, turn_id: TurnId) -> String {
    format!(
        "{}/api/v1/sessions/{session_id}/turns/{turn_id}/cancel",
        server.base_url
    )
}

#[tokio::test]
async fn cancel_endpoint_validates_version_and_preserves_active_turn_when_canceling_queue()
-> anyhow::Result<()> {
    let directory = TempDir::new()?;
    let state = AppState::initialize(test_config(directory.path().into())).await?;
    let seeded = seed_session(&state, true).await?;
    let queued_turn_id = seeded.queued_turn_id.expect("queued Turn fixture");
    let server = spawn(state.clone()).await?;
    let client = Client::new();
    let url = cancel_url(&server, seeded.session_id, queued_turn_id);

    let missing_version = client
        .post(&url)
        .json(&json!({ "reason": "user_cancel" }))
        .send()
        .await?;
    assert_eq!(missing_version.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        missing_version
            .text()
            .await?
            .contains("expected_session_version")
    );

    let stale_version = client
        .post(&url)
        .json(&json!({
            "expected_session_version": "v_stale",
            "reason": "user_cancel"
        }))
        .send()
        .await?;
    assert_eq!(stale_version.status(), StatusCode::PRECONDITION_FAILED);
    let problem: Value = stale_version.json().await?;
    assert_eq!(problem["code"], "RESOURCE_VERSION_MISMATCH");

    let accepted = client
        .post(&url)
        .json(&json!({
            "expected_session_version": "v_session",
            "reason": "user_cancel"
        }))
        .send()
        .await?;
    assert_eq!(accepted.status(), StatusCode::OK);
    let response: Value = accepted.json().await?;
    assert_eq!(response["data"]["turn_id"], queued_turn_id.to_string());
    assert_eq!(response["data"]["from_status"], "queued");
    assert_eq!(response["data"]["to_status"], "canceled");
    let next_version = response["data"]["session_version"]
        .as_str()
        .expect("Session version in cancellation response");
    assert_ne!(next_version, "v_session");

    let pool = state.pool();
    let queued_status: String = sqlx::query_scalar("SELECT status FROM turns WHERE id = ?")
        .bind(queued_turn_id.to_string())
        .fetch_one(pool)
        .await?;
    assert_eq!(queued_status, "canceled");
    let session: (String, Option<String>, String) =
        sqlx::query_as("SELECT state, active_turn_id, version FROM sessions WHERE id = ?")
            .bind(seeded.session_id.to_string())
            .fetch_one(pool)
            .await?;
    assert_eq!(session.0, "active");
    assert_eq!(session.1, Some(seeded.active_turn_id.to_string()));
    assert_eq!(session.2, next_version);

    Ok(())
}

#[tokio::test]
async fn cancel_endpoint_settles_active_turn_and_releases_session() -> anyhow::Result<()> {
    let directory = TempDir::new()?;
    let state = AppState::initialize(test_config(directory.path().into())).await?;
    let seeded = seed_session(&state, false).await?;
    let server = spawn(state.clone()).await?;
    let client = Client::new();

    let accepted = client
        .post(cancel_url(
            &server,
            seeded.session_id,
            seeded.active_turn_id,
        ))
        .json(&json!({
            "expected_session_version": "v_session",
            "reason": "user_cancel"
        }))
        .send()
        .await?;
    assert_eq!(accepted.status(), StatusCode::OK);
    let response: Value = accepted.json().await?;
    assert_eq!(
        response["data"]["turn_id"],
        seeded.active_turn_id.to_string()
    );
    assert_eq!(response["data"]["from_status"], "running");
    assert_eq!(response["data"]["to_status"], "canceled");
    let next_version = response["data"]["session_version"]
        .as_str()
        .expect("Session version in cancellation response");
    assert_ne!(next_version, "v_session");

    let pool = state.pool();
    let turn: (String, Option<String>) =
        sqlx::query_as("SELECT status, cancellation_reason FROM turns WHERE id = ?")
            .bind(seeded.active_turn_id.to_string())
            .fetch_one(pool)
            .await?;
    assert_eq!(turn, ("canceled".into(), Some("user_cancel".into())));
    let session: (String, Option<String>, String) =
        sqlx::query_as("SELECT state, active_turn_id, version FROM sessions WHERE id = ?")
            .bind(seeded.session_id.to_string())
            .fetch_one(pool)
            .await?;
    assert_eq!(session, ("ready".into(), None, next_version.into()));

    Ok(())
}

#[tokio::test]
async fn a_new_turn_keeps_durable_messages_from_a_canceled_turn() -> anyhow::Result<()> {
    let directory = TempDir::new()?;
    let state = AppState::initialize(test_config(directory.path().into())).await?;
    let seeded = seed_session(&state, false).await?;
    let pool = state.pool();
    let prior_turn_id = seeded.active_turn_id;
    let next_turn_id = TurnId::new();

    sqlx::query(
        "UPDATE turns SET status = 'canceled', cancellation_reason = 'user_cancel', updated_at = ? \
         WHERE id = ?",
    )
    .bind(NOW)
    .bind(prior_turn_id.to_string())
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO turns \
         (id, session_id, sequence, status, model_snapshot_json, version, created_at, updated_at) \
         VALUES (?, ?, 2, 'running', '{}', 'v_turn_2', ?, ?)",
    )
    .bind(next_turn_id.to_string())
    .bind(seeded.session_id.to_string())
    .bind(NOW)
    .bind(NOW)
    .execute(pool)
    .await?;
    sqlx::query(
        "UPDATE sessions SET state = 'active', active_turn_id = ?, version = 'v_session_2' \
         WHERE id = ?",
    )
    .bind(next_turn_id.to_string())
    .bind(seeded.session_id.to_string())
    .execute(pool)
    .await?;

    for (id, turn_id, kind, sequence, text) in [
        (
            "message-canceled-user",
            prior_turn_id.to_string(),
            "user",
            1_i64,
            "The canceled request still matters",
        ),
        (
            "message-canceled-assistant",
            prior_turn_id.to_string(),
            "assistant",
            2_i64,
            "The canceled turn had useful context",
        ),
    ] {
        sqlx::query(
            "INSERT INTO messages \
             (id, session_id, turn_id, actor_json, kind, body_json, status, \
              timeline_sequence, version, created_at) \
             VALUES (?, ?, ?, '{}', ?, ?, 'active', ?, ?, ?)",
        )
        .bind(id)
        .bind(seeded.session_id.to_string())
        .bind(turn_id)
        .bind(kind)
        .bind(json!({"parts": [{"type": "text", "text": text}]}).to_string())
        .bind(sequence)
        .bind(format!("v_{id}"))
        .bind(NOW)
        .execute(pool)
        .await?;
    }

    let context = state
        .sessions()
        .context_messages(seeded.session_id, next_turn_id)
        .await?;
    let context_text: Vec<&str> = context
        .iter()
        .filter_map(|message| {
            message
                .body
                .get("parts")
                .and_then(Value::as_array)
                .and_then(|parts| parts.first())
                .and_then(|part| part.get("text"))
                .and_then(Value::as_str)
        })
        .collect();
    assert_eq!(
        context_text,
        vec![
            "The canceled request still matters",
            "The canceled turn had useful context"
        ]
    );

    Ok(())
}
