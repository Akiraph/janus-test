//! Stage 6 context assembly + Compact scheduling.
//!
//! Owns the durable `context_versions` projection used by each Round. Stage 6
//! ships the ledger side (one row per assembled context, with a stable
//! `system_prefix_version` and `estimated_input_tokens`) and the manual
//! `schedule_compact` path that records an immutable `compact_summaries` row.
//! Real token estimating and automatic compaction scheduling are deferred (see
//! implement.md Stage 6); the surface here lets a Round attach a context version
//! and lets a user request a compact without rebuilding the chat history.

use serde_json::{Value, json};
use sqlx::Row;
use sqlx::SqlitePool;

use crate::platform::{
    clock::{Clock, SystemClock, format_utc},
    id::{CompactSummaryId, ContextVersionId, SessionId},
};

/// Stable system prefix; bump when SYSTEM_PROMPT changes so old
/// `context_versions.system_prefix_version` rows are distinguishable.
pub const SYSTEM_PREFIX_VERSION: &str = "sys-1";

/// Bytes of the durable system prefix prepended to every Turn's first Round.
pub(crate) const SYSTEM_PROMPT: &str = "You are the Janus Supervisor coding agent. \
You may only use registered tools on the Session workspace. \
Call finish with the completion summary, main changes, performed and unperformed validation, and remaining risks when the user request is complete. \
Do not attempt Apply, Sync, Git write, or Main workspace access.";

/// Record one `context_versions` row for a Session/Turn and return its id.
/// `estimated_input_tokens` is a coarse token estimate used to drive later
/// automatic Compact scheduling; in M4 it is a rough `chars/4` of the
/// assembled history so the column is populated for downstream decisions.
pub async fn record_context_version(
    pool: &SqlitePool,
    session_id: SessionId,
    turn_id: Option<&str>,
    estimated_input_tokens: i64,
    context_limit: i64,
    compact_status: &str,
    selection_json: Value,
) -> anyhow::Result<String> {
    let id = ContextVersionId::new().to_string();
    let now = format_utc(SystemClock.now());
    sqlx::query(
        "INSERT INTO context_versions \
         (id, session_id, turn_id, sequence, compact_summary_id, system_prefix_version, \
          estimated_input_tokens, context_limit, compact_status, selection_json, created_at) \
         VALUES (?, ?, ?, \
          (SELECT COALESCE(MAX(sequence), 0) + 1 FROM context_versions WHERE session_id = ?), \
          NULL, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(session_id.to_string())
    .bind(turn_id)
    .bind(session_id.to_string())
    .bind(SYSTEM_PREFIX_VERSION)
    .bind(estimated_input_tokens)
    .bind(context_limit)
    .bind(compact_status)
    .bind(selection_json.to_string())
    .bind(&now)
    .execute(pool)
    .await?;
    Ok(id)
}

/// Schedule a manual Compact for a Session: persist one immutable
/// `compact_summaries` row covering the supplied timeline range, and mark the
/// session's latest `context_versions` row `compact_status = 'scheduled'` so
/// the next Round can surface the compacted summary. Returns the summary id.
///
/// Note: in M4 this records the durable intent and the immutable summary
/// ledger; actually replacing the chat history prefix on the next Round is
/// deferred (see implement.md Stage 6). Keeping the row immutable means even a
/// partial wiring can never corrupt prior context.
pub async fn schedule_compact(
    pool: &SqlitePool,
    session_id: SessionId,
    source_first: Option<&str>,
    source_last: &str,
    summary: Value,
) -> anyhow::Result<String> {
    let id = CompactSummaryId::new().to_string();
    let now = format_utc(SystemClock.now());
    sqlx::query(
        "INSERT INTO compact_summaries \
         (id, session_id, source_first_timeline_id, source_last_timeline_id, summary_json, \
          model_attempt_id, input_tokens, output_tokens, created_at) \
         VALUES (?, ?, ?, ?, ?, NULL, 0, 0, ?)",
    )
    .bind(&id)
    .bind(session_id.to_string())
    .bind(source_first)
    .bind(source_last)
    .bind(summary.to_string())
    .bind(&now)
    .execute(pool)
    .await?;
    // Mark the latest context_version for this session as scheduled.
    sqlx::query(
        "UPDATE context_versions \
         SET compact_status = 'scheduled', compact_summary_id = ? \
         WHERE id = (SELECT id FROM context_versions \
                     WHERE session_id = ? ORDER BY sequence DESC LIMIT 1)",
    )
    .bind(&id)
    .bind(session_id.to_string())
    .execute(pool)
    .await?;
    Ok(id)
}

/// Load the latest compact summary for a session, if any. Used by the next
/// Round's context assembly to decide whether a compacted prefix should be
/// prepended instead of the raw history. Returns `(summary_json, source_last)`.
pub async fn latest_compact_summary(
    pool: &SqlitePool,
    session_id: SessionId,
) -> anyhow::Result<Option<(Value, String)>> {
    let row = sqlx::query(
        "SELECT summary_json, source_last_timeline_id FROM compact_summaries \
         WHERE session_id = ? ORDER BY created_at DESC LIMIT 1",
    )
    .bind(session_id.to_string())
    .fetch_optional(pool)
    .await?;
    if let Some(row) = row {
        let s: String = row.try_get("summary_json")?;
        let last: String = row.try_get("source_last_timeline_id")?;
        let v: Value = serde_json::from_str(&s).unwrap_or(json!({}));
        Ok(Some((v, last)))
    } else {
        Ok(None)
    }
}
