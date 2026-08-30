//! MongoDB collection and index catalog for the Janus data model.
//!
//! This file replaces the SQL migration files: creating a fresh database is a
//! per-collection `create_indexes` pass (idempotent) plus pre-creating the
//! collections that carry no index. The xtask governance pass reads this
//! catalog to verify collection ownership, so keep `COLLECTIONS` and the owner
//! comments here in sync with each module's `module.toml`.

use mongodb::{
    IndexModel,
    bson::{Document, doc},
    options::IndexOptions,
};

/// Highest applied schema shape. The old SQLite migrator recorded version 4
/// (the last migration file); keep the same number so callers that surface it
/// do not see a regression.
pub const SCHEMA_VERSION: i64 = 4;

/// Every collection the server can touch as `(name, owning module)`. The
/// platform module is owned by infrastructure + apps/server core. All other
/// names must match one `module.toml` `owned_collections` entry.
pub const COLLECTIONS: &[(&str, &str)] = &[
    // platform (infrastructure + server core)
    ("public_events", "platform"),
    ("projection_cursor", "platform"),
    ("event_seq", "platform"),
    ("operations", "platform"),
    ("operation_steps", "platform"),
    ("work_items", "platform"),
    ("idempotency_records", "platform"),
    ("command_idempotency_records", "platform"),
    ("blob_objects", "platform"),
    ("blob_references", "platform"),
    ("blob_cleanup_intents", "platform"),
    // identity
    ("owners", "identity"),
    ("initialization_tokens", "identity"),
    ("passkeys", "identity"),
    ("ceremonies", "identity"),
    ("login_sessions", "identity"),
    ("recovery_batches", "identity"),
    ("recovery_codes", "identity"),
    ("recovery_states", "identity"),
    // models
    ("model_providers", "models"),
    ("models", "models"),
    ("model_failover", "models"),
    ("model_attempts", "models"),
    ("model_usage_ledger", "models"),
    ("automation_settings", "models"),
    // projects
    ("projects", "projects"),
    ("github_credentials", "projects"),
    ("memories", "projects"),
    // source-control
    ("project_git_state", "source-control"),
    ("git_update_conflicts", "source-control"),
    ("git_update_conflict_paths", "source-control"),
    // runtime
    ("runtimes", "runtime"),
    ("log_streams", "runtime"),
    ("async_tasks", "runtime"),
    ("terminals", "runtime"),
    ("runtime_access_tickets", "runtime"),
    // sessions
    ("sessions", "sessions"),
    ("turns", "sessions"),
    ("messages", "sessions"),
    ("timeline_items", "sessions"),
    ("checkpoints", "sessions"),
    ("uploads", "sessions"),
    ("attachments", "sessions"),
    ("message_attachments", "sessions"),
    // execution
    ("rounds", "execution"),
    ("tool_calls", "execution"),
    ("plan_versions", "execution"),
    ("compact_summaries", "execution"),
    ("context_versions", "execution"),
    // workspace
    ("workspace_copies", "workspace"),
    ("content_revisions", "workspace"),
    ("workspace_snapshots", "workspace"),
    ("workspace_mutation_intents", "workspace"),
    // notifications
    ("notification_channels", "notifications"),
];

/// Collections with no index. `create_indexes` implicitly creates a collection,
/// but an index-less collection would never materialize, so these are created
/// explicitly at open time. `event_seq` is the event cursor counter singleton.
pub const INDEXLESS_COLLECTIONS: &[&str] = &[
    "owners",
    "ceremonies",
    "recovery_batches",
    "automation_settings",
    "project_git_state",
    "projection_cursor",
    "event_seq",
    "command_idempotency_records",
];

fn index(name: &str, keys: Document) -> IndexModel {
    IndexModel::builder()
        .keys(keys)
        .options(IndexOptions::builder().name(name.to_owned()).build())
        .build()
}

fn unique_index(name: &str, keys: Document) -> IndexModel {
    IndexModel::builder()
        .keys(keys)
        .options(
            IndexOptions::builder()
                .name(name.to_owned())
                .unique(true)
                .build(),
        )
        .build()
}

fn partial_index(name: &str, keys: Document, filter: Document) -> IndexModel {
    IndexModel::builder()
        .keys(keys)
        .options(
            IndexOptions::builder()
                .name(name.to_owned())
                .partial_filter_expression(filter)
                .build(),
        )
        .build()
}

fn unique_partial_index(name: &str, keys: Document, filter: Document) -> IndexModel {
    IndexModel::builder()
        .keys(keys)
        .options(
            IndexOptions::builder()
                .name(name.to_owned())
                .unique(true)
                .partial_filter_expression(filter)
                .build(),
        )
        .build()
}

/// A status `IN (...) -> partial filter`. The `$or`/`$eq` expansion keeps the
/// partial index compatible with MongoDB 5+, where `$in` in a partial filter
/// was only accepted from 7.0.
fn status_in(statuses: &[&str]) -> Document {
    doc! {
        "$or": statuses.iter().map(|status| {
            doc! {"status": {"$eq": status}}
        }).collect::<Vec<_>>()
    }
}

/// All collection indexes as `(collection, [IndexModel])`. Index names mirror
/// the old SQL index names where one existed; composite-primary-key tables get
/// a `_pk` unique index in place of the SQL PRIMARY KEY constraint.
pub fn index_specs() -> Vec<(&'static str, Vec<IndexModel>)> {
    vec![
        (
            "public_events",
            vec![
                index(
                    "public_events_type_cursor_idx",
                    doc! {"event_type": 1, "_id": 1},
                ),
                unique_index("public_events_event_id_idx", doc! {"event_id": 1}),
            ],
        ),
        (
            "operations",
            vec![
                index("operations_status_idx", doc! {"status": 1}),
                index(
                    "operations_target_idx",
                    doc! {"target_kind": 1, "target_id": 1},
                ),
            ],
        ),
        (
            "operation_steps",
            vec![unique_index(
                "operation_steps_pk",
                doc! {"operation_id": 1, "step_key": 1},
            )],
        ),
        (
            "work_items",
            vec![partial_index(
                "work_items_claimable_idx",
                doc! {"dead": 1, "not_before": 1},
                doc! {"lease_expires_at": null},
            )],
        ),
        (
            "idempotency_records",
            vec![index(
                "idempotency_records_owner_idx",
                doc! {"owner_id": 1, "expires_at": 1},
            )],
        ),
        (
            "blob_objects",
            vec![index("blob_objects_state_idx", doc! {"storage_state": 1})],
        ),
        (
            "blob_references",
            vec![
                unique_index(
                    "blob_references_pk",
                    doc! {"owner_module": 1, "owner_type": 1, "owner_id": 1, "purpose": 1},
                ),
                index("blob_references_blob_idx", doc! {"blob_sha": 1}),
            ],
        ),
        (
            "blob_cleanup_intents",
            vec![
                unique_index(
                    "blob_cleanup_intents_owner_idx",
                    doc! {"owner_module": 1, "owner_type": 1, "owner_id": 1, "purpose": 1},
                ),
                index(
                    "blob_cleanup_intents_due_idx",
                    doc! {"next_attempt_at": 1, "updated_at": 1},
                ),
            ],
        ),
        (
            "initialization_tokens",
            vec![unique_index(
                "initialization_tokens_token_hash_idx",
                doc! {"token_hash": 1},
            )],
        ),
        (
            "passkeys",
            vec![unique_index(
                "passkeys_credential_idx",
                doc! {"credential_json": 1},
            )],
        ),
        (
            "login_sessions",
            vec![unique_index(
                "login_sessions_token_hash_idx",
                doc! {"token_hash": 1},
            )],
        ),
        (
            "recovery_codes",
            vec![unique_index(
                "recovery_codes_code_hash_idx",
                doc! {"code_hash": 1},
            )],
        ),
        (
            "recovery_states",
            vec![unique_index(
                "recovery_states_token_hash_idx",
                doc! {"token_hash": 1},
            )],
        ),
        (
            "model_providers",
            vec![unique_index(
                "model_provider_client_name_idx",
                doc! {"owner_id": 1, "client": 1, "display_name": 1},
            )],
        ),
        (
            "models",
            vec![
                unique_index(
                    "models_provider_display_idx",
                    doc! {"provider_id": 1, "display_name": 1},
                ),
                unique_index(
                    "models_provider_upstream_idx",
                    doc! {"provider_id": 1, "upstream_model_id": 1},
                ),
            ],
        ),
        (
            "model_failover",
            vec![
                unique_index(
                    "model_failover_pk",
                    doc! {"primary_model_id": 1, "candidate_model_id": 1},
                ),
                unique_index(
                    "model_failover_ordinal_idx",
                    doc! {"primary_model_id": 1, "ordinal": 1},
                ),
            ],
        ),
        (
            "model_attempts",
            vec![index(
                "model_attempts_round_idx",
                doc! {"round_id": 1, "candidate_order": 1, "attempt_number": 1},
            )],
        ),
        (
            "model_usage_ledger",
            vec![
                index("model_usage_ledger_attempt_idx", doc! {"attempt_id": 1}),
                index(
                    "model_usage_ledger_project_idx",
                    doc! {"project_id": 1, "occurred_at": 1},
                ),
                index(
                    "model_usage_ledger_session_idx",
                    doc! {"session_id": 1, "occurred_at": 1},
                ),
            ],
        ),
        (
            "projects",
            vec![
                index(
                    "projects_owner_idx",
                    doc! {"owner_id": 1, "last_activity_at": 1},
                ),
                index("projects_state_idx", doc! {"state": 1}),
            ],
        ),
        (
            "github_credentials",
            vec![unique_index(
                "github_credential_name_idx",
                doc! {"owner_id": 1, "name": 1},
            )],
        ),
        (
            "memories",
            vec![unique_index(
                "memories_pk",
                doc! {"project_id": 1, "memory_key": 1},
            )],
        ),
        (
            "git_update_conflicts",
            vec![index(
                "git_update_conflicts_project_idx",
                doc! {"project_id": 1, "state": 1},
            )],
        ),
        (
            "git_update_conflict_paths",
            vec![unique_index(
                "git_update_conflict_paths_pk",
                doc! {"conflict_id": 1, "path": 1},
            )],
        ),
        (
            "runtimes",
            vec![unique_partial_index(
                "runtimes_one_current_per_scope",
                doc! {"scope_kind": 1, "scope_id": 1},
                status_in(&["starting", "ready", "stopping"]),
            )],
        ),
        (
            "log_streams",
            vec![unique_index(
                "log_streams_owner_idx",
                doc! {"owner_kind": 1, "owner_id": 1},
            )],
        ),
        (
            "async_tasks",
            vec![
                index(
                    "async_tasks_session_idx",
                    doc! {"session_id": 1, "created_at": 1},
                ),
                index(
                    "async_tasks_turn_idx",
                    doc! {"controlling_turn_id": 1, "status": 1},
                ),
            ],
        ),
        (
            "terminals",
            vec![index(
                "terminals_owner_idx",
                doc! {"owner_kind": 1, "owner_id": 1, "created_at": 1},
            )],
        ),
        (
            "runtime_access_tickets",
            vec![
                unique_index(
                    "runtime_access_tickets_token_hash_idx",
                    doc! {"token_hash": 1},
                ),
                index(
                    "runtime_access_tickets_terminal_idx",
                    doc! {"terminal_id": 1, "expires_at": 1},
                ),
            ],
        ),
        (
            "sessions",
            vec![
                index(
                    "sessions_project_idx",
                    doc! {"project_id": 1, "last_activity_at": 1},
                ),
                index("sessions_state_idx", doc! {"state": 1}),
            ],
        ),
        (
            "turns",
            vec![
                unique_index("turns_session_idx", doc! {"session_id": 1, "sequence": 1}),
                partial_index(
                    "turns_queued_idx",
                    doc! {"session_id": 1, "sequence": 1},
                    doc! {"status": "queued"},
                ),
                unique_partial_index(
                    "turns_one_active_per_session",
                    doc! {"session_id": 1},
                    status_in(&["running", "canceling"]),
                ),
            ],
        ),
        (
            "messages",
            vec![
                index(
                    "messages_session_idx",
                    doc! {"session_id": 1, "created_at": 1},
                ),
                index("messages_turn_idx", doc! {"turn_id": 1}),
            ],
        ),
        (
            "timeline_items",
            vec![
                index(
                    "timeline_items_session_order_idx",
                    doc! {"session_id": 1, "display_order": 1},
                ),
                index("timeline_items_turn_idx", doc! {"turn_id": 1}),
            ],
        ),
        (
            "checkpoints",
            vec![index(
                "checkpoints_session_idx",
                doc! {"session_id": 1, "created_at": 1},
            )],
        ),
        (
            "uploads",
            vec![index(
                "uploads_owner_idx",
                doc! {"owner_id": 1, "created_at": 1},
            )],
        ),
        (
            "attachments",
            vec![index("attachments_session_idx", doc! {"session_id": 1})],
        ),
        (
            "message_attachments",
            vec![
                unique_index(
                    "message_attachments_pk",
                    doc! {"message_id": 1, "attachment_id": 1},
                ),
                index(
                    "message_attachments_attachment_idx",
                    doc! {"attachment_id": 1},
                ),
            ],
        ),
        (
            "rounds",
            vec![unique_index(
                "rounds_turn_idx",
                doc! {"turn_id": 1, "sequence": 1},
            )],
        ),
        (
            "tool_calls",
            vec![
                unique_index("tool_calls_round_idx", doc! {"round_id": 1, "ord": 1}),
                unique_partial_index(
                    "tool_calls_provider_call_idx",
                    doc! {"round_id": 1, "provider_call_id": 1},
                    // MongoDB partial filters reject $ne (it desugars to $not);
                    // $gt: null keeps the SQL `provider_call_id IS NOT NULL`
                    // semantics — missing fields compare as null, and null is
                    // below every real BSON value.
                    doc! {"provider_call_id": {"$gt": null}},
                ),
            ],
        ),
        (
            "plan_versions",
            vec![unique_index(
                "plan_versions_turn_idx",
                doc! {"turn_id": 1, "sequence": 1},
            )],
        ),
        (
            "compact_summaries",
            vec![index(
                "compact_summaries_session_idx",
                doc! {"session_id": 1, "created_at": 1},
            )],
        ),
        (
            "context_versions",
            vec![unique_index(
                "context_versions_session_idx",
                doc! {"session_id": 1, "sequence": 1},
            )],
        ),
        (
            "workspace_copies",
            vec![index(
                "workspace_copies_project_idx",
                doc! {"project_id": 1},
            )],
        ),
        (
            "content_revisions",
            vec![unique_index(
                "content_revisions_handle_idx",
                doc! {"workspace_handle": 1, "sequence": 1},
            )],
        ),
        (
            "workspace_snapshots",
            vec![unique_index(
                "workspace_snapshots_revision_idx",
                doc! {"revision_id": 1},
            )],
        ),
        (
            "workspace_mutation_intents",
            vec![
                index(
                    "workspace_mutation_intents_handle_idx",
                    doc! {"workspace_handle": 1, "state": 1},
                ),
                index(
                    "workspace_mutation_intents_recovery_idx",
                    doc! {"state": 1, "updated_at": 1},
                ),
            ],
        ),
        (
            "notification_channels",
            vec![index(
                "notification_channels_owner_idx",
                doc! {"owner_id": 1, "enabled": 1, "display_name": 1},
            )],
        ),
    ]
}

/// Indexes every collection listed in `COLLECTIONS` is expected to carry, minus
/// the index-less ones. Used by xtask to catch a collection whose indexes were
/// dropped without also removing it from the catalog.
pub fn collection_index_count(name: &str) -> usize {
    index_specs()
        .into_iter()
        .find_map(|(collection, models)| (collection == name).then_some(models.len()))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::{COLLECTIONS, INDEXLESS_COLLECTIONS, collection_index_count};

    #[test]
    fn every_collection_is_either_indexed_or_explicitly_indexless() {
        for (name, _owner) in COLLECTIONS {
            let indexed = collection_index_count(name) > 0;
            let indexless = INDEXLESS_COLLECTIONS.contains(name);
            assert!(
                indexed != indexless,
                "collection {name} must be exactly one of indexed or indexless"
            );
        }
    }

    #[test]
    fn no_duplicate_collection_names() {
        let names = COLLECTIONS
            .iter()
            .map(|(name, _)| name.to_owned())
            .collect::<BTreeSet<_>>();
        assert_eq!(names.len(), COLLECTIONS.len());
    }
}
