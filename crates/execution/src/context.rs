//! Durable context-version and compact-summary records used by each Round.
//!
//! The interface records immutable context intent and a coarse token estimate.
//! Rebuilding the chat prefix remains an application decision, so this module
//! does not silently rewrite prior context history.

use janus_infrastructure::clock::now_utc_str;
use serde_json::{Value, json};
use sqlx::{Row, SqliteConnection, SqlitePool};

use janus_infrastructure::id::{CompactSummaryId, ContextVersionId, SessionId};

pub const DEFAULT_CONTEXT_LIMIT: i64 = 1_000_000;
pub const AUTO_COMPACT_THRESHOLD_PERCENT: i64 = 90;

#[derive(Debug)]
pub struct ScheduleCompactInput {
    pub session_id: SessionId,
    pub compact_summary_id: String,
    pub source_first: Option<String>,
    pub source_last: String,
    pub summary: Value,
    pub estimated_input_tokens: i64,
    pub context_limit: i64,
}

pub fn context_usage_near_limit(estimated_input_tokens: i64, context_limit: i64) -> bool {
    context_limit > 0
        && i128::from(estimated_input_tokens.max(0)) * 100
            >= i128::from(context_limit) * AUTO_COMPACT_THRESHOLD_PERCENT as i128
}

/// Bytes of the durable system prefix prepended to every Turn's first Round.
pub(crate) const SYSTEM_PROMPT: &str = r#"Goal:
Complete the user's request end to end, grounded in evidence from tool results and durable state.

Constraints:
- Treat tool results and durable state as authoritative.
- Do not claim changes or validation that were not actually performed.
- Work in the current project repository. Use the file tools for repository
  files and Bash for normal shell commands, Git, builds, and tests.

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
- If evidence is insufficient to support a conclusion, say so explicitly rather than guessing; request only information that cannot be safely discovered.
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
    let mut connection = pool.acquire().await?;
    record_context_version_in_tx(
        &mut connection,
        session_id,
        turn_id,
        estimated_input_tokens,
        context_limit,
        compact_status,
        selection_json,
    )
    .await
}

pub async fn record_context_version_in_tx(
    tx: &mut SqliteConnection,
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
    .execute(&mut *tx)
    .await?;
    Ok(id)
}

/// Schedule a manual Compact for a Session: persist one immutable
/// `compact_summaries` row covering the supplied timeline range, and mark the
/// session's latest `context_versions` row `compact_status = 'scheduled'` so
/// the next Round can surface the compacted summary. Returns the summary id.
///
/// This records durable intent and an immutable summary ledger. Replacing the
/// chat history prefix remains a separate execution decision, so partial
/// wiring cannot corrupt prior context.
pub async fn schedule_compact(
    pool: &SqlitePool,
    session_id: SessionId,
    source_first: Option<&str>,
    source_last: &str,
    summary: Value,
) -> anyhow::Result<String> {
    let mut connection = pool.acquire().await?;
    let id = CompactSummaryId::new().to_string();
    schedule_compact_in_tx(
        &mut connection,
        ScheduleCompactInput {
            session_id,
            compact_summary_id: id,
            source_first: source_first.map(ToOwned::to_owned),
            source_last: source_last.to_owned(),
            summary,
            estimated_input_tokens: 0,
            context_limit: DEFAULT_CONTEXT_LIMIT,
        },
    )
    .await
}

pub async fn schedule_compact_in_tx(
    tx: &mut SqliteConnection,
    input: ScheduleCompactInput,
) -> anyhow::Result<String> {
    let ScheduleCompactInput {
        session_id,
        compact_summary_id,
        source_first,
        source_last,
        summary,
        estimated_input_tokens,
        context_limit,
    } = input;
    let now = now_utc_str();
    sqlx::query(
        "INSERT INTO compact_summaries \
         (id, session_id, source_first_timeline_id, source_last_timeline_id, summary_json, \
          model_attempt_id, input_tokens, output_tokens, created_at) \
         VALUES (?, ?, ?, ?, ?, NULL, 0, 0, ?)",
    )
    .bind(&compact_summary_id)
    .bind(session_id.to_string())
    .bind(&source_first)
    .bind(&source_last)
    .bind(summary.to_string())
    .bind(&now)
    .execute(&mut *tx)
    .await?;
    let updated = sqlx::query(
        "UPDATE context_versions \
         SET compact_status = 'scheduled', compact_summary_id = ? \
         WHERE id = (SELECT id FROM context_versions \
                     WHERE session_id = ? ORDER BY sequence DESC LIMIT 1)",
    )
    .bind(&compact_summary_id)
    .bind(session_id.to_string())
    .execute(&mut *tx)
    .await?;
    if updated.rows_affected() == 0 {
        let context_version_id = record_context_version_in_tx(
            tx,
            session_id,
            None,
            estimated_input_tokens,
            context_limit,
            "scheduled",
            json!({"kind": "manual_compact"}),
        )
        .await?;
        sqlx::query("UPDATE context_versions SET compact_summary_id = ? WHERE id = ?")
            .bind(&compact_summary_id)
            .bind(context_version_id)
            .execute(&mut *tx)
            .await?;
    } else {
        sqlx::query(
            "UPDATE context_versions SET estimated_input_tokens = ?, context_limit = ? \
             WHERE compact_summary_id = ? AND session_id = ?",
        )
        .bind(estimated_input_tokens)
        .bind(context_limit)
        .bind(&compact_summary_id)
        .bind(session_id.to_string())
        .execute(&mut *tx)
        .await?;
    }
    Ok(compact_summary_id)
}

pub async fn begin_compact_in_tx(
    tx: &mut SqliteConnection,
    session_id: SessionId,
    compact_summary_id: &str,
) -> anyhow::Result<bool> {
    let changed = sqlx::query(
        "UPDATE context_versions SET compact_status = 'running' \
         WHERE session_id = ? AND compact_summary_id = ? AND compact_status = 'scheduled'",
    )
    .bind(session_id.to_string())
    .bind(compact_summary_id)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    Ok(changed == 1)
}

pub async fn complete_compact_in_tx(
    tx: &mut SqliteConnection,
    session_id: SessionId,
    compact_summary_id: &str,
    estimated_input_tokens: i64,
) -> anyhow::Result<bool> {
    let changed = sqlx::query(
        "UPDATE context_versions \
         SET compact_status = 'succeeded', estimated_input_tokens = ? \
         WHERE session_id = ? AND compact_summary_id = ? \
           AND compact_status IN ('scheduled', 'running')",
    )
    .bind(estimated_input_tokens)
    .bind(session_id.to_string())
    .bind(compact_summary_id)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    Ok(changed == 1)
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
         WHERE session_id = ? AND EXISTS(SELECT 1 FROM context_versions \
             WHERE context_versions.session_id = compact_summaries.session_id \
               AND context_versions.compact_summary_id = compact_summaries.id \
               AND context_versions.compact_status = 'succeeded') \
         ORDER BY created_at DESC, id DESC LIMIT 1",
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

#[cfg(test)]
mod tests {
    use super::{AUTO_COMPACT_THRESHOLD_PERCENT, SYSTEM_PROMPT, context_usage_near_limit};

    #[test]
    fn system_prompt_names_the_project_repository_boundary() {
        assert!(SYSTEM_PROMPT.contains("current project repository"));
        assert!(SYSTEM_PROMPT.contains("file tools"));
        assert!(SYSTEM_PROMPT.contains("Bash"));
    }

    #[test]
    fn automatic_compact_starts_at_the_context_threshold() {
        let limit = 1_000_000;
        let threshold = limit * AUTO_COMPACT_THRESHOLD_PERCENT / 100;
        assert!(!context_usage_near_limit(threshold - 1, limit));
        assert!(context_usage_near_limit(threshold, limit));
        assert!(context_usage_near_limit(limit, limit));
        assert!(!context_usage_near_limit(1, 0));
    }
}
