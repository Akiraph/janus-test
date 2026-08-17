//! Idempotency records for synchronous application commands.
//!
//! Durable operations already have their own idempotency journal. Message,
//! steer and cancel commands return a small synchronous result instead, so
//! they use this separate table and store the exact response that was
//! committed with the command's transaction.

use anyhow::anyhow;
use serde_json::Value;
use sqlx::SqliteConnection;

use crate::{clock::now_utc_str, operations::IdempotencyRequest};

pub async fn lookup_in_tx(
    connection: &mut SqliteConnection,
    request: &IdempotencyRequest,
) -> anyhow::Result<Option<Value>> {
    let row: Option<(String, String, String, String, String)> = sqlx::query_as(
        "SELECT owner_id, method, normalized_route, request_digest, response_json \
         FROM command_idempotency_records WHERE key = ?",
    )
    .bind(&request.key)
    .fetch_optional(&mut *connection)
    .await?;

    let Some((owner_id, method, route, digest, response_json)) = row else {
        return Ok(None);
    };
    if owner_id != request.owner_id
        || method != request.method
        || route != request.normalized_route
        || digest != request.digest
    {
        return Err(anyhow!("IDEMPOTENCY_KEY_REUSED"));
    }
    Ok(Some(serde_json::from_str(&response_json)?))
}

pub async fn record_in_tx(
    connection: &mut SqliteConnection,
    request: &IdempotencyRequest,
    response: &Value,
) -> anyhow::Result<()> {
    sqlx::query(
        "INSERT INTO command_idempotency_records \
         (key, owner_id, method, normalized_route, request_digest, response_json, expires_at, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&request.key)
    .bind(&request.owner_id)
    .bind(&request.method)
    .bind(&request.normalized_route)
    .bind(&request.digest)
    .bind(serde_json::to_string(response)?)
    .bind(&request.expires_at)
    .bind(now_utc_str())
    .execute(&mut *connection)
    .await?;
    Ok(())
}
