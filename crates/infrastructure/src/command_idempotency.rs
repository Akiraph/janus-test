//! Idempotency records for synchronous application commands.
//!
//! Durable operations already have their own idempotency journal. Message,
//! steer and cancel commands return a small synchronous result instead, so
//! they use this separate table and store the exact response that was
//! committed with the command's transaction.

use anyhow::anyhow;
use mongodb::{
    ClientSession,
    bson::{Document, doc},
};
use serde_json::Value;

use crate::{clock::now_utc_str, operations::IdempotencyRequest};

pub async fn lookup_in_tx(
    database: &mongodb::Database,
    session: &mut ClientSession,
    request: &IdempotencyRequest,
) -> anyhow::Result<Option<Value>> {
    let document = database
        .collection::<Document>("command_idempotency_records")
        .find_one(doc! {"_id": &request.key})
        .session(&mut *session)
        .await?;
    let Some(document) = document else {
        return Ok(None);
    };
    if document.get_str("owner_id")? != request.owner_id
        || document.get_str("method")? != request.method
        || document.get_str("normalized_route")? != request.normalized_route
        || document.get_str("request_digest")? != request.digest
    {
        return Err(anyhow!("IDEMPOTENCY_KEY_REUSED"));
    }
    Ok(Some(serde_json::from_str(
        document.get_str("response_json")?,
    )?))
}

pub async fn record_in_tx(
    database: &mongodb::Database,
    session: &mut ClientSession,
    request: &IdempotencyRequest,
    response: &Value,
) -> anyhow::Result<()> {
    let response_json = serde_json::to_string(response)?;
    database
        .collection::<Document>("command_idempotency_records")
        .insert_one(doc! {
            "_id": &request.key,
            "owner_id": &request.owner_id,
            "method": &request.method,
            "normalized_route": &request.normalized_route,
            "request_digest": &request.digest,
            "response_json": &response_json,
            "expires_at": &request.expires_at,
            "created_at": now_utc_str(),
        })
        .session(&mut *session)
        .await?;
    Ok(())
}
