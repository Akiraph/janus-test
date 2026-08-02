//! Durable context-version and compact-summary records used by each Round.
//!
//! The interface records immutable context intent and a coarse token estimate.
//! Rebuilding the chat prefix remains an application decision, so this module
//! does not silently rewrite prior context history.

use janus_infrastructure::clock::now_utc_str;
use serde_json::{Value, json};
use sqlx::Row;
use sqlx::SqlitePool;

use janus_infrastructure::id::{CompactSummaryId, ContextVersionId, SessionId};

/// Bytes of the durable system prefix prepended to every Turn's first Round.
pub(crate) const SYSTEM_PROMPT: &str = r#"Goal:
Complete the user's request end to end, grounded in evidence from tool results and durable state.

Constraints:
- Treat tool results and durable state as authoritative.
- Do not claim changes or validation that were not actually performed.

Success criteria:
- The requested outcome is implemented, or the exact blocker is identified.
- Relevant validation is run when available.
- Unperformed validation, remaining risks, and follow-up work are stated explicitly.

Verification (before reporting complete):
- Re-check the result against every constraint above.
- Run available validation for key changes; report what was run and what was not.
- Distinguish verified facts from inferences and unknowns.

Output:
Respond like a coding teammate, not a report: lead with the outcome, mention key files and verification when code changed, and say plainly when verification was not run. When complete, cover completed work, main file changes, validation performed, validation not performed, remaining risks or TODOs. A normal final response ends the Turn; do not call a completion tool. Do not use Markdown headings or standalone section labels — Janus does not render them well; use short paragraphs and only a few flat bullets when helpful.

Stop rules:
- Stop when success criteria are met.
- If evidence is insufficient to support a conclusion, say so explicitly rather than guessing; ask only for information that cannot be safely discovered.
- Do not broaden the requested scope without user authorization."#;

/// Record one `context_versions` row for a Session/Turn and return its id.
/// `estimated_input_tokens` is a coarse token estimate used to drive later
/// automatic Compact scheduling; it is a rough `chars/4` of the
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
    let now = now_utc_str();
    sqlx::query(
        "INSERT INTO context_versions \
         (id, session_id, turn_id, sequence, compact_summary_id, \
          estimated_input_tokens, context_limit, compact_status, selection_json, created_at) \
         VALUES (?, ?, ?, \
          (SELECT COALESCE(MAX(sequence), 0) + 1 FROM context_versions WHERE session_id = ?), \
          NULL, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(session_id.to_string())
    .bind(turn_id)
    .bind(session_id.to_string())
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
/// This records durable intent and an immutable summary ledger.
/// Replacing the chat history prefix remains a separate execution decision.
/// Keeping the row immutable means partial wiring cannot corrupt prior context.
/// partial wiring can never corrupt prior context.
pub async fn schedule_compact(
    pool: &SqlitePool,
    session_id: SessionId,
    source_first: Option<&str>,
    source_last: &str,
    summary: Value,
) -> anyhow::Result<String> {
    let id = CompactSummaryId::new().to_string();
    let now = now_utc_str();
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
