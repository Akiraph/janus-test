//! Stage 6: ContextAssembler + manual Compact scheduling.
//!
//! Exercises the durable `context_versions` / `compact_summaries` ledger that
//! Stage 6 lands. Real token estimation and automatic compaction are deferred
//! (implement.md Stage 6); the wire-able manual `schedule_compact` path and the
//! context-version record are tested here so the surface is stable for later
//! Round wiring.

mod support;

use janus_server::modules::supervisor::context::{
    SYSTEM_PREFIX_VERSION, latest_compact_summary, record_context_version, schedule_compact,
};
use janus_server::platform::database::Database;
use janus_server::platform::id::SessionId;
use serde_json::json;
use tempfile::TempDir;

async fn boot() -> anyhow::Result<(TempDir, sqlx::SqlitePool)> {
    let temp = TempDir::new()?;
    let db = Database::open(temp.path()).await?;
    Ok((temp, db.pool().clone()))
}

#[tokio::test]
async fn record_context_version_advances_sequence() -> anyhow::Result<()> {
    let (_t, pool) = boot().await?;
    let sid = SessionId::new();
    let id1 = record_context_version(
        &pool,
        sid,
        None,
        100,
        200_000,
        "not_needed",
        json!({"turn": "round-1"}),
    )
    .await?;
    let id2 = record_context_version(
        &pool,
        sid,
        None,
        250,
        200_000,
        "not_needed",
        json!({"turn": "round-2"}),
    )
    .await?;
    assert_ne!(id1, id2);
    let rows: Vec<(String, String, i64, String)> = sqlx::query_as(
        "SELECT id, system_prefix_version, estimated_input_tokens, compact_status \
         FROM context_versions WHERE session_id = ? ORDER BY sequence",
    )
    .bind(sid.to_string())
    .fetch_all(&pool)
    .await?;
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].1, SYSTEM_PREFIX_VERSION);
    assert_eq!(rows[0].2, 100);
    assert_eq!(rows[1].3, "not_needed");
    Ok(())
}

#[tokio::test]
async fn schedule_compact_records_immutable_summary() -> anyhow::Result<()> {
    let (_t, pool) = boot().await?;
    let sid = SessionId::new();
    // Seed a context version first; compact targets the latest one.
    record_context_version(
        &pool,
        sid,
        None,
        500,
        200_000,
        "not_needed",
        json!({"note": "precompact"}),
    )
    .await?;
    let summary = json!({"done": "refactored auth module", "open": []});
    let id = schedule_compact(
        &pool,
        sid,
        Some("tl-first"),
        "tl-last",
        summary.clone(),
    )
    .await?;
    assert!(!id.is_empty());

    let latest = latest_compact_summary(&pool, sid).await?.expect("summary recorded");
    assert_eq!(latest.0, summary);
    assert_eq!(latest.1, "tl-last");

    // The summary row is immutable: scheduling again records a new row, never
    // overwriting the prior one. Range lookup always returns the newest.
    let id2 = schedule_compact(
        &pool,
        sid,
        Some("tl-first-2"),
        "tl-last-2",
        json!({"done": "second pass"}),
    )
    .await?;
    assert_ne!(id, id2);
    let all: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM compact_summaries WHERE session_id = ?",
    )
    .bind(sid.to_string())
    .fetch_one(&pool)
    .await?;
    assert_eq!(all, 2);
    let latest = latest_compact_summary(&pool, sid).await?.expect("summary recorded");
    assert_eq!(latest.1, "tl-last-2");
    Ok(())
}

#[tokio::test]
async fn schedule_compact_marks_context_scheduled() -> anyhow::Result<()> {
    let (_t, pool) = boot().await?;
    let sid = SessionId::new();
    record_context_version(&pool, sid, None, 1000, 200_000, "not_needed", json!({})).await?;
    schedule_compact(&pool, sid, None, "tl-x", json!({"s": 1})).await?;
    let status: String = sqlx::query_scalar(
        "SELECT compact_status FROM context_versions WHERE session_id = ? \
         ORDER BY sequence DESC LIMIT 1",
    )
    .bind(sid.to_string())
    .fetch_one(&pool)
    .await?;
    assert_eq!(status, "scheduled");
    let linked: Option<String> = sqlx::query_scalar(
        "SELECT compact_summary_id FROM context_versions WHERE session_id = ? \
         ORDER BY sequence DESC LIMIT 1",
    )
    .bind(sid.to_string())
    .fetch_optional(&pool)
    .await?;
    assert!(linked.is_some() && !linked.unwrap().is_empty());
    Ok(())
}
