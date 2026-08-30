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
use mongodb::bson::{Bson, Document, doc};
use reqwest::{Client, StatusCode};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::{net::TcpListener, task::JoinHandle};

const NOW: &str = "2026-07-31T00:00:00.000Z";

static TEST_DB_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn test_config(data_root: PathBuf) -> Config {
    Config {
        bind: SocketAddr::from(([127, 0, 0, 1], 0)),
        data_root,
        web_dist: None,
        mode: RunMode::Development,
        development_auth: true,
        auth_mode: janus_identity::AuthMode::Passkey,
        webauthn_rp_name: "Janus Test".into(),
        webauthn_rp_id: "localhost".into(),
        public_origin: url::Url::parse("http://localhost").expect("static test URL"),
        event_heartbeat: Duration::from_millis(50),
        automation_webhook_enabled: false,
        automation_webhook_secret: None,
        automation_github_token: None,
        mongodb_uri: std::env::var("JANUS_MONGODB_URI")
            .unwrap_or_else(|_| "mongodb://localhost:27017/?replicaSet=rs0".into()),
        mongodb_database: format!(
            "janus_test_{}_{}",
            std::process::id(),
            TEST_DB_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ),
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
    let db = state.pool();
    let project_id = ProjectId::new();
    let session_id = SessionId::new();
    let active_turn_id = TurnId::new();
    let queued_turn_id = include_queued_turn.then(TurnId::new);

    db.collection::<Document>("owners")
        .insert_one(doc! {
            "_id": "owner-cancel-test",
            "display_name": "Owner",
            "created_at": NOW,
        })
        .await?;
    db.collection::<Document>("projects")
        .insert_one(doc! {
            "_id": project_id.to_string(),
            "owner_id": "owner-cancel-test",
            "name": "Project",
            "state": "ready",
            "repo_access": "public_https",
            "repo_url": "https://example.com/repo.git",
            "repo_branch": null,
            "github_credential_id": null,
            "default_model_id": null,
            "main_workspace_handle": null,
            "clone_error": null,
            "version": "v_project",
            "created_at": NOW,
            "updated_at": NOW,
            "last_activity_at": NOW,
        })
        .await?;
    db.collection::<Document>("sessions")
        .insert_one(doc! {
            "_id": session_id.to_string(),
            "project_id": project_id.to_string(),
            "title": null,
            "state": "active",
            "next_model_ref": null,
            "active_turn_id": active_turn_id.to_string(),
            "version": "v_session",
            "created_at": NOW,
            "updated_at": NOW,
            "last_activity_at": NOW,
        })
        .await?;
    db.collection::<Document>("turns")
        .insert_one(doc! {
            "_id": active_turn_id.to_string(),
            "session_id": session_id.to_string(),
            "sequence": 1i64,
            "status": "running",
            "input_message_id": null,
            "model_snapshot_json": "{}",
            "goal_mode": 0i64,
            "predecessor_turn_id": null,
            "completion_reason": null,
            "cancellation_reason": null,
            "version": "v_turn_1",
            "created_at": NOW,
            "updated_at": NOW,
        })
        .await?;
    if let Some(turn_id) = queued_turn_id {
        db.collection::<Document>("turns")
            .insert_one(doc! {
                "_id": turn_id.to_string(),
                "session_id": session_id.to_string(),
                "sequence": 2i64,
                "status": "queued",
                "input_message_id": null,
                "model_snapshot_json": "{}",
                "goal_mode": 0i64,
                "predecessor_turn_id": null,
                "completion_reason": null,
                "cancellation_reason": null,
                "version": "v_turn_2",
                "created_at": NOW,
                "updated_at": NOW,
            })
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
    let config = test_config(directory.path().into());
    let active_turn_id;
    {
        let state = AppState::initialize(config.clone()).await?;
        active_turn_id = seed_session(&state, false).await?.active_turn_id;
    }

    let state = AppState::initialize(config).await?;
    let db = state.pool();
    let turn = db
        .collection::<Document>("turns")
        .find_one(doc! {"_id": active_turn_id.to_string()})
        .await?
        .expect("seeded active turn exists");
    let status = turn.get_str("status")?.to_owned();
    let session = db
        .collection::<Document>("sessions")
        .find_one(doc! {"_id": turn.get_str("session_id")?})
        .await?
        .expect("session owning the seeded turn exists");
    let active_session_turn = session
        .get("active_turn_id")
        .and_then(Bson::as_str)
        .map(str::to_owned);
    assert_eq!(status, "running");
    assert_eq!(
        active_session_turn.as_deref(),
        Some(active_turn_id.to_string().as_str())
    );

    let work = db
        .collection::<Document>("work_items")
        .find_one(doc! {"handler_kind": "turn.execute"})
        .sort(doc! {"created_at": -1})
        .await?
        .expect("a turn.execute wake work item should be enqueued");
    let handler_kind = work.get_str("handler_kind")?.to_owned();
    let payload_json = work.get_str("payload_json")?.to_owned();
    assert_eq!(handler_kind, "turn.execute");
    let payload: Value = serde_json::from_str(&payload_json)?;
    assert_eq!(payload["turn_id"], active_turn_id.to_string());

    Ok(())
}

#[tokio::test]
async fn work_queue_bounds_attempts_and_dead_letters_failures() -> anyhow::Result<()> {
    let directory = TempDir::new()?;
    let state = AppState::initialize(test_config(directory.path().into())).await?;
    let db = state.pool();
    let now = janus_infrastructure::clock::now_utc_str();
    db.collection::<Document>("work_items")
        .insert_one(doc! {
            "_id": "bounded-work",
            "handler_kind": "test.failure",
            "payload_json": "{}",
            "not_before": &now,
            "lease_nonce": null,
            "lease_expires_at": null,
            "attempts": 0i64,
            "dead": false,
            "created_at": &now,
        })
        .await?;

    for expected_attempt in 1..=5_i64 {
        let claimed = state
            .operations()
            .claim_work("test.failure", 60)
            .await?
            .expect("work item should remain claimable until the fifth attempt");
        let work = db
            .collection::<Document>("work_items")
            .find_one(doc! {"_id": "bounded-work"})
            .await?
            .expect("bounded work item exists");
        let attempts = work.get_i64("attempts")?;
        assert_eq!(attempts, expected_attempt);
        assert!(
            state
                .operations()
                .fail_work(&claimed.id, &claimed.nonce, WorkFailureDisposition::Retry,)
                .await?
        );
        if expected_attempt < 5 {
            // Bypass the real delay so the test exercises the bound quickly.
            db.collection::<Document>("work_items")
                .update_one(
                    doc! {"_id": "bounded-work"},
                    doc! {"$set": {"not_before": janus_infrastructure::clock::now_utc_str()}},
                )
                .await?;
        }
    }

    let work = db
        .collection::<Document>("work_items")
        .find_one(doc! {"_id": "bounded-work"})
        .await?
        .expect("bounded work item exists");
    let attempts = work.get_i64("attempts")?;
    let dead = work.get_bool("dead")?;
    assert_eq!(attempts, 5);
    assert!(dead);

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

    state
        .pool()
        .collection::<Document>("work_items")
        .update_many(
            doc! {},
            doc! {"$set": {"lease_expires_at": "2000-01-01T00:00:00.000Z"}},
        )
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

    let db = state.pool();
    let queued = db
        .collection::<Document>("turns")
        .find_one(doc! {"_id": queued_turn_id.to_string()})
        .await?
        .expect("queued turn exists");
    let queued_status = queued.get_str("status")?.to_owned();
    assert_eq!(queued_status, "canceled");
    let session = db
        .collection::<Document>("sessions")
        .find_one(doc! {"_id": seeded.session_id.to_string()})
        .await?
        .expect("session exists");
    let session_state = session.get_str("state")?.to_owned();
    let active_session_turn = session
        .get("active_turn_id")
        .and_then(Bson::as_str)
        .map(str::to_owned);
    let session_version = session.get_str("version")?.to_owned();
    assert_eq!(session_state, "active");
    assert_eq!(active_session_turn, Some(seeded.active_turn_id.to_string()));
    assert_eq!(session_version, next_version);

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

    let db = state.pool();
    let turn = db
        .collection::<Document>("turns")
        .find_one(doc! {"_id": seeded.active_turn_id.to_string()})
        .await?
        .expect("turn exists");
    let turn_status = turn.get_str("status")?.to_owned();
    let cancellation_reason = turn
        .get("cancellation_reason")
        .and_then(Bson::as_str)
        .map(str::to_owned);
    assert_eq!(turn_status, "canceled");
    assert_eq!(cancellation_reason.as_deref(), Some("user_cancel"));
    let session = db
        .collection::<Document>("sessions")
        .find_one(doc! {"_id": seeded.session_id.to_string()})
        .await?
        .expect("session exists");
    let session_state = session.get_str("state")?.to_owned();
    let active_session_turn = session
        .get("active_turn_id")
        .and_then(Bson::as_str)
        .map(str::to_owned);
    let session_version = session.get_str("version")?.to_owned();
    assert_eq!(session_state, "ready");
    assert_eq!(active_session_turn, None);
    assert_eq!(session_version, next_version);

    Ok(())
}

#[tokio::test]
async fn a_new_turn_keeps_durable_messages_from_a_canceled_turn() -> anyhow::Result<()> {
    let directory = TempDir::new()?;
    let state = AppState::initialize(test_config(directory.path().into())).await?;
    let seeded = seed_session(&state, false).await?;
    let db = state.pool();
    let prior_turn_id = seeded.active_turn_id;
    let next_turn_id = TurnId::new();

    db.collection::<Document>("turns")
        .update_one(
            doc! {"_id": prior_turn_id.to_string()},
            doc! {
                "$set": {
                    "status": "canceled",
                    "cancellation_reason": "user_cancel",
                    "updated_at": NOW,
                }
            },
        )
        .await?;
    db.collection::<Document>("turns")
        .insert_one(doc! {
            "_id": next_turn_id.to_string(),
            "session_id": seeded.session_id.to_string(),
            "sequence": 2i64,
            "status": "running",
            "input_message_id": null,
            "model_snapshot_json": "{}",
            "goal_mode": 0i64,
            "predecessor_turn_id": null,
            "completion_reason": null,
            "cancellation_reason": null,
            "version": "v_turn_2",
            "created_at": NOW,
            "updated_at": NOW,
        })
        .await?;
    db.collection::<Document>("sessions")
        .update_one(
            doc! {"_id": seeded.session_id.to_string()},
            doc! {
                "$set": {
                    "state": "active",
                    "active_turn_id": next_turn_id.to_string(),
                    "version": "v_session_2",
                }
            },
        )
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
        db.collection::<Document>("messages")
            .insert_one(doc! {
                "_id": id,
                "session_id": seeded.session_id.to_string(),
                "turn_id": turn_id,
                "actor_json": "{}",
                "kind": kind,
                "body_json": json!({"parts": [{"type": "text", "text": text}]}).to_string(),
                "status": "active",
                "timeline_sequence": sequence,
                "version": format!("v_{id}"),
                "created_at": NOW,
            })
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
