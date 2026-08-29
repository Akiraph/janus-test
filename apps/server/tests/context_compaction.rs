//! Context assembly and manual Compact scheduling.
//!
//! Exercises the durable `context_versions` / `compact_summaries` ledger that
//! supports compaction. The manual `schedule_compact` path and context-version
//! record are tested independently of automatic Round wiring.

use futures_util::TryStreamExt;
use janus_execution::interface::{
    latest_compact_summary, record_context_version, schedule_compact,
};
use janus_infrastructure::{database::Database, id::SessionId};
use mongodb::bson::{Document, doc};
use serde_json::json;
use tempfile::TempDir;

static TEST_DB_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

async fn boot() -> anyhow::Result<(TempDir, mongodb::Database)> {
    let temp = TempDir::new()?;
    let uri = std::env::var("JANUS_MONGODB_URI")
        .unwrap_or_else(|_| "mongodb://localhost:27017/?replicaSet=rs0".to_owned());
    let name = format!(
        "janus_test_{}_{}",
        std::process::id(),
        TEST_DB_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    );
    let db = Database::open(temp.path(), &uri, &name).await?;
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
    let mut cursor = pool
        .collection::<Document>("context_versions")
        .find(doc! {"session_id": sid.to_string()})
        .sort(doc! {"sequence": 1})
        .await?;
    let mut rows: Vec<(String, i64, String)> = Vec::new();
    while let Some(document) = cursor.try_next().await? {
        rows.push((
            document.get_str("_id")?.to_owned(),
            document.get_i64("estimated_input_tokens")?,
            document.get_str("compact_status")?.to_owned(),
        ));
    }
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].1, 100);
    assert_eq!(rows[1].2, "not_needed");
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
    let id = schedule_compact(&pool, sid, Some("tl-first"), "tl-last", summary.clone()).await?;
    assert!(!id.is_empty());

    let stored = pool
        .collection::<Document>("compact_summaries")
        .find_one(doc! {"_id": &id})
        .await?
        .expect("compact summary recorded");
    let stored_summary = stored.get_str("summary_json")?.to_owned();
    let stored_last = stored.get_str("source_last_timeline_id")?.to_owned();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&stored_summary)?,
        summary
    );
    assert_eq!(stored_last, "tl-last");
    assert!(latest_compact_summary(&pool, sid).await?.is_none());

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
    let all = pool
        .collection::<Document>("compact_summaries")
        .count_documents(doc! {"session_id": sid.to_string()})
        .await?;
    assert_eq!(all, 2);
    pool.collection::<Document>("context_versions")
        .update_one(
            doc! {
                "session_id": sid.to_string(),
                "compact_summary_id": &id2,
            },
            doc! {"$set": {"compact_status": "succeeded"}},
        )
        .await?;
    let latest = latest_compact_summary(&pool, sid)
        .await?
        .expect("succeeded summary recorded");
    assert_eq!(latest.1, "tl-last-2");
    Ok(())
}

#[tokio::test]
async fn schedule_compact_marks_context_scheduled() -> anyhow::Result<()> {
    let (_t, pool) = boot().await?;
    let sid = SessionId::new();
    record_context_version(&pool, sid, None, 1000, 200_000, "not_needed", json!({})).await?;
    schedule_compact(&pool, sid, None, "tl-x", json!({"s": 1})).await?;
    let status_doc = pool
        .collection::<Document>("context_versions")
        .find_one(doc! {"session_id": sid.to_string()})
        .sort(doc! {"sequence": -1})
        .await?
        .expect("context version recorded");
    let status = status_doc.get_str("compact_status")?.to_owned();
    assert_eq!(status, "scheduled");
    let linked = pool
        .collection::<Document>("context_versions")
        .find_one(doc! {"session_id": sid.to_string()})
        .sort(doc! {"sequence": -1})
        .await?
        .and_then(|document| {
            document
                .get_str("compact_summary_id")
                .ok()
                .map(str::to_owned)
        });
    assert!(linked.is_some_and(|id| !id.is_empty()));
    Ok(())
}
