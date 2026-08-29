//! Durable context-version and compact-summary records used by each Round.
//!
//! The interface records immutable context intent and a coarse token estimate.
//! Rebuilding the chat prefix remains an application decision, so this module
//! does not silently rewrite prior context history.

use std::collections::HashSet;

use futures_util::TryStreamExt;
use janus_infrastructure::{
    clock::now_utc_str,
    id::{CompactSummaryId, ContextVersionId, SessionId},
};
use mongodb::{
    ClientSession,
    bson::{Bson, Document, doc},
};
use serde_json::{Value, json};

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

/// Record one `context_versions` document for a Session/Turn and return its id.
/// `estimated_input_tokens` is a coarse token estimate used to drive later
/// automatic Compact scheduling; it is a rough `chars/4` of the
/// assembled history so the column is populated for downstream decisions.
pub async fn record_context_version(
    pool: &mongodb::Database,
    session_id: SessionId,
    turn_id: Option<&str>,
    estimated_input_tokens: i64,
    context_limit: i64,
    compact_status: &str,
    selection_json: Value,
) -> anyhow::Result<String> {
    let mut session = pool.client().start_session().await?;
    session.start_transaction().await?;
    let id = record_context_version_in_tx(
        pool,
        &mut session,
        session_id,
        turn_id,
        estimated_input_tokens,
        context_limit,
        compact_status,
        selection_json,
    )
    .await?;
    session.commit_transaction().await?;
    Ok(id)
}

pub async fn record_context_version_in_tx(
    database: &mongodb::Database,
    session: &mut ClientSession,
    session_id: SessionId,
    turn_id: Option<&str>,
    estimated_input_tokens: i64,
    context_limit: i64,
    compact_status: &str,
    selection_json: Value,
) -> anyhow::Result<String> {
    let id = ContextVersionId::new().to_string();
    let now = now_utc_str();
    let latest = database
        .collection::<Document>("context_versions")
        .find_one(doc! {"session_id": session_id.to_string()})
        .sort(doc! {"sequence": -1})
        .session(&mut *session)
        .await?;
    let sequence = latest
        .and_then(|doc| doc.get("sequence").and_then(Bson::as_i64))
        .unwrap_or(0)
        .saturating_add(1);
    let selection_json_str = selection_json.to_string();
    let mut document = doc! {
        "_id": &id,
        "session_id": session_id.to_string(),
        "sequence": sequence,
        "estimated_input_tokens": estimated_input_tokens,
        "context_limit": context_limit,
        "compact_status": compact_status,
        "selection_json": &selection_json_str,
        "created_at": &now,
    };
    if let Some(turn_id) = turn_id {
        document.insert("turn_id", turn_id);
    }
    database
        .collection::<Document>("context_versions")
        .insert_one(document)
        .session(&mut *session)
        .await?;
    Ok(id)
}

/// Schedule a manual Compact for a Session: persist one immutable
/// `compact_summaries` document covering the supplied timeline range, and mark
/// the session's latest `context_versions` document `compact_status = 'scheduled'`
/// so the next Round can surface the compacted summary. Returns the summary id.
///
/// This records durable intent and an immutable summary ledger. Replacing the
/// chat history prefix remains a separate execution decision, so partial
/// wiring cannot corrupt prior context.
pub async fn schedule_compact(
    pool: &mongodb::Database,
    session_id: SessionId,
    source_first: Option<&str>,
    source_last: &str,
    summary: Value,
) -> anyhow::Result<String> {
    let mut session = pool.client().start_session().await?;
    session.start_transaction().await?;
    let id = CompactSummaryId::new().to_string();
    schedule_compact_in_tx(
        pool,
        &mut session,
        ScheduleCompactInput {
            session_id,
            compact_summary_id: id.clone(),
            source_first: source_first.map(ToOwned::to_owned),
            source_last: source_last.to_owned(),
            summary,
            estimated_input_tokens: 0,
            context_limit: DEFAULT_CONTEXT_LIMIT,
        },
    )
    .await?;
    session.commit_transaction().await?;
    Ok(id)
}

pub async fn schedule_compact_in_tx(
    database: &mongodb::Database,
    session: &mut ClientSession,
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
    let summary_json = summary.to_string();
    let mut summary_document = doc! {
        "_id": &compact_summary_id,
        "session_id": session_id.to_string(),
        "source_last_timeline_id": &source_last,
        "summary_json": &summary_json,
        "input_tokens": 0i64,
        "output_tokens": 0i64,
        "created_at": &now,
    };
    if let Some(source_first) = source_first {
        summary_document.insert("source_first_timeline_id", source_first);
    }
    database
        .collection::<Document>("compact_summaries")
        .insert_one(summary_document)
        .session(&mut *session)
        .await?;
    let latest = database
        .collection::<Document>("context_versions")
        .find_one(doc! {"session_id": session_id.to_string()})
        .sort(doc! {"sequence": -1})
        .session(&mut *session)
        .await?;
    let updated = match latest {
        Some(document) => {
            let id = document.get_str("_id")?.to_owned();
            database
                .collection::<Document>("context_versions")
                .update_one(
                    doc! {"_id": &id},
                    doc! {
                        "$set": {
                            "compact_status": "scheduled",
                            "compact_summary_id": &compact_summary_id,
                        }
                    },
                )
                .session(&mut *session)
                .await?
                .matched_count
        }
        None => 0,
    };
    if updated == 0 {
        let context_version_id = record_context_version_in_tx(
            database,
            session,
            session_id,
            None,
            estimated_input_tokens,
            context_limit,
            "scheduled",
            json!({"kind": "manual_compact"}),
        )
        .await?;
        database
            .collection::<Document>("context_versions")
            .update_one(
                doc! {"_id": context_version_id},
                doc! {"$set": {"compact_summary_id": &compact_summary_id}},
            )
            .session(&mut *session)
            .await?;
    } else {
        database
            .collection::<Document>("context_versions")
            .update_many(
                doc! {
                    "compact_summary_id": &compact_summary_id,
                    "session_id": session_id.to_string(),
                },
                doc! {
                    "$set": {
                        "estimated_input_tokens": estimated_input_tokens,
                        "context_limit": context_limit,
                    }
                },
            )
            .session(&mut *session)
            .await?;
    }
    Ok(compact_summary_id)
}

pub async fn begin_compact_in_tx(
    database: &mongodb::Database,
    session: &mut ClientSession,
    session_id: SessionId,
    compact_summary_id: &str,
) -> anyhow::Result<bool> {
    let changed = database
        .collection::<Document>("context_versions")
        .update_one(
            doc! {
                "session_id": session_id.to_string(),
                "compact_summary_id": compact_summary_id,
                "compact_status": "scheduled",
            },
            doc! {"$set": {"compact_status": "running"}},
        )
        .session(&mut *session)
        .await?
        .matched_count;
    Ok(changed == 1)
}

pub async fn complete_compact_in_tx(
    database: &mongodb::Database,
    session: &mut ClientSession,
    session_id: SessionId,
    compact_summary_id: &str,
    estimated_input_tokens: i64,
) -> anyhow::Result<bool> {
    let changed = database
        .collection::<Document>("context_versions")
        .update_one(
            doc! {
                "session_id": session_id.to_string(),
                "compact_summary_id": compact_summary_id,
                "compact_status": {"$in": ["scheduled", "running"]},
            },
            doc! {
                "$set": {
                    "compact_status": "succeeded",
                    "estimated_input_tokens": estimated_input_tokens,
                }
            },
        )
        .session(&mut *session)
        .await?
        .matched_count;
    Ok(changed == 1)
}

/// Backfill a compact summary document with the model-generated summary and
/// its real token usage once the summary attempt settles. The document is
/// created at schedule time with the placeholder digest; this records what the
/// model actually produced and how much it cost.
pub async fn finalize_compact_summary_in_tx(
    database: &mongodb::Database,
    session: &mut ClientSession,
    compact_summary_id: &str,
    summary: Value,
    model_attempt_id: Option<&str>,
    input_tokens: i64,
    output_tokens: i64,
) -> anyhow::Result<()> {
    let summary_json = summary.to_string();
    let mut set = doc! {
        "summary_json": &summary_json,
        "input_tokens": input_tokens,
        "output_tokens": output_tokens,
    };
    if let Some(model_attempt_id) = model_attempt_id {
        set.insert("model_attempt_id", model_attempt_id);
    }
    database
        .collection::<Document>("compact_summaries")
        .update_one(doc! {"_id": compact_summary_id}, doc! {"$set": set})
        .session(&mut *session)
        .await?;
    Ok(())
}

/// Load the latest compact summary for a session, if any. Used by the next
/// Round's context assembly to decide whether a compacted prefix should be
/// prepended instead of the raw history. Returns `(summary_json, source_last)`.
pub async fn latest_compact_summary(
    pool: &mongodb::Database,
    session_id: SessionId,
) -> anyhow::Result<Option<(Value, String)>> {
    // Succeeded context versions referencing a compact summary for this session.
    let mut versions = pool
        .collection::<Document>("context_versions")
        .find(doc! {
            "session_id": session_id.to_string(),
            "compact_status": "succeeded",
        })
        .await?;
    let mut summary_ids = HashSet::new();
    while let Some(document) = versions.try_next().await? {
        if let Ok(id) = document.get_str("compact_summary_id") {
            summary_ids.insert(id.to_owned());
        }
    }
    if summary_ids.is_empty() {
        return Ok(None);
    }
    let ids: Vec<&str> = summary_ids.iter().map(String::as_str).collect();
    let document = pool
        .collection::<Document>("compact_summaries")
        .find_one(doc! {
            "session_id": session_id.to_string(),
            "_id": {"$in": ids},
        })
        .sort(doc! {"created_at": -1, "_id": -1})
        .await?;
    let Some(document) = document else {
        return Ok(None);
    };
    let summary_json = document.get_str("summary_json")?;
    let summary: Value = serde_json::from_str(summary_json).unwrap_or(json!({}));
    let last = document.get_str("source_last_timeline_id")?.to_owned();
    Ok(Some((summary, last)))
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
