//! Content-addressed blob store (SHA-256 CAS).
//!
//! Registers references for GC roots and performs conservative mark-and-sweep
//! collection. Only objects with no logical reference can reach `trash`.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use crate::clock::{format_utc, now_utc, now_utc_str};
use anyhow::Context;
use sha2::{Digest, Sha256};
use sqlx::SqlitePool;
use tokio::io::AsyncWriteExt;

use crate::{id::BlobSha, random_hex_token};

fn shard(sha: &str) -> String {
    sha.chars().take(2).collect()
}

fn object_path(objects_root: &Path, sha: &str) -> PathBuf {
    objects_root.join("sha256").join(shard(sha)).join(sha)
}

fn hex_encode(bytes: impl AsRef<[u8]>) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let bytes = bytes.as_ref();
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

#[derive(Clone)]
pub struct BlobStore {
    pool: SqlitePool,
    objects_root: PathBuf,
    incoming_root: PathBuf,
    storage_lock: Arc<tokio::sync::Mutex<()>>,
}

impl BlobStore {
    pub fn new(pool: SqlitePool, data_root: &Path) -> anyhow::Result<Self> {
        let objects_root = data_root.join("objects");
        let incoming_root = objects_root.join("incoming");
        std::fs::create_dir_all(&objects_root)
            .with_context(|| format!("create objects root {}", objects_root.display()))?;
        std::fs::create_dir_all(&incoming_root)
            .with_context(|| format!("create incoming root {}", incoming_root.display()))?;
        Ok(Self {
            pool,
            objects_root,
            incoming_root,
            storage_lock: Arc::new(tokio::sync::Mutex::new(())),
        })
    }

    /// Write bytes to the content-addressed store and register one logical reference.
    ///
    /// The file becomes durable before the reference transaction commits. Keeping
    /// the temporary and final paths on one filesystem makes the rename atomic;
    /// a crash can therefore leave recoverable debris but not a dangling reference.
    pub async fn write(&self, bytes: &[u8], reference: BlobReference) -> anyhow::Result<BlobSha> {
        let _storage_lock = self.storage_lock.lock().await;
        let sha = hex_sha256(bytes);
        let target = object_path(&self.objects_root, &sha);

        // Never trust a matching filename without checking the recorded length.
        if tokio::fs::try_exists(&target).await.unwrap_or(false) {
            self.verify_length(&sha, bytes.len()).await?;
            self.register_reference(&sha, bytes.len(), reference)
                .await?;
            return Ok(BlobSha::from_hex(sha));
        }

        let incoming = self
            .incoming_root
            .join(format!("{}.tmp", random_hex_token()));
        {
            let mut file = tokio::fs::File::create(&incoming)
                .await
                .with_context(|| format!("create incoming temp {}", incoming.display()))?;
            file.write_all(bytes).await?;
            file.sync_all().await?;
        }
        if let Some(parent) = target.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .with_context(|| format!("create shard dir {}", parent.display()))?;
        }
        tokio::fs::rename(&incoming, &target)
            .await
            .with_context(|| format!("atomic rename to {}", target.display()))?;
        if let Some(parent) = target.parent() {
            // Windows may reject directory handles. The file itself is already synced,
            // so a best-effort parent sync is the only portable option here.
            if let Ok(dir) = tokio::fs::File::open(parent).await {
                let _ = dir.sync_all().await;
            }
        }

        self.register_reference(&sha, bytes.len(), reference)
            .await?;
        Ok(BlobSha::from_hex(sha))
    }

    /// Read the raw bytes for an object. Caller is responsible for size limits.
    pub async fn read(&self, sha: &str) -> anyhow::Result<Vec<u8>> {
        let path = object_path(&self.objects_root, sha);
        Ok(tokio::fs::read(&path).await?)
    }

    /// Verify a present object's stored length matches the recorded size.
    pub async fn verify_length(&self, sha: &str, expected: usize) -> anyhow::Result<()> {
        let row =
            sqlx::query_scalar::<_, i64>("SELECT byte_size FROM blob_objects WHERE sha256 = ?")
                .bind(sha)
                .fetch_optional(&self.pool)
                .await?;
        match row {
            Some(recorded) => {
                if usize::try_from(recorded)? == expected {
                    Ok(())
                } else {
                    anyhow::bail!("blob {sha} length mismatch: db={recorded} expected={expected}")
                }
            }
            None => {
                // A crash can leave the renamed object before its DB transaction.
                // Reconstruct only the object record; the caller still adds its reference.
                let path = object_path(&self.objects_root, sha);
                let meta = tokio::fs::metadata(&path).await?;
                let size = i64::try_from(meta.len())?;
                let now = now_utc_str();
                sqlx::query(
                    "INSERT OR IGNORE INTO blob_objects (sha256, byte_size, storage_state, first_written_at) VALUES (?, ?, 'present', ?)",
                )
                .bind(sha)
                .bind(size)
                .bind(&now)
                .execute(&self.pool)
                .await?;
                Ok(())
            }
        }
    }

    async fn register_reference(
        &self,
        sha: &str,
        size: usize,
        reference: BlobReference,
    ) -> anyhow::Result<()> {
        let now = now_utc_str();
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "INSERT OR IGNORE INTO blob_objects (sha256, byte_size, storage_state, first_written_at) VALUES (?, ?, 'present', ?)",
        )
        .bind(sha)
        .bind(i64::try_from(size)?)
        .bind(&now)
        .execute(&mut *tx)
        .await?;
        sqlx::query("INSERT OR IGNORE INTO blob_references (owner_module, owner_type, owner_id, purpose, blob_sha, created_at) VALUES (?, ?, ?, ?, ?, ?)")
            .bind(reference.owner_module)
            .bind(reference.owner_type)
            .bind(reference.owner_id.as_str())
            .bind(reference.purpose)
            .bind(sha)
            .bind(&now)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Remove a single logical reference. Returns true if the reference existed.
    pub async fn drop_reference(&self, reference: &BlobReference) -> anyhow::Result<bool> {
        let result = sqlx::query("DELETE FROM blob_references WHERE owner_module = ? AND owner_type = ? AND owner_id = ? AND purpose = ?")
            .bind(reference.owner_module)
            .bind(reference.owner_type)
            .bind(reference.owner_id.as_str())
            .bind(reference.purpose)
            .execute(&self.pool)
            .await;
        match result {
            Ok(result) => Ok(result.rows_affected() > 0),
            Err(error) => {
                self.enqueue_cleanup(reference, &error.to_string()).await?;
                Err(error.into())
            }
        }
    }

    /// Retry durable reference deletions recorded after a failed cleanup.
    /// Startup callers should fail readiness if this returns an error so a
    /// database outage cannot silently accumulate ownership drift.
    pub async fn recover_cleanup(&self) -> anyhow::Result<usize> {
        let now = now_utc_str();
        let rows: Vec<(String, String, String, String, String)> = sqlx::query_as(
            "SELECT id, owner_module, owner_type, owner_id, purpose \
             FROM blob_cleanup_intents WHERE next_attempt_at <= ? \
             ORDER BY next_attempt_at, updated_at, id",
        )
        .bind(&now)
        .fetch_all(&self.pool)
        .await?;
        let mut recovered = 0;
        for (id, owner_module, owner_type, owner_id, purpose) in rows {
            match sqlx::query(
                "DELETE FROM blob_references WHERE owner_module = ? AND owner_type = ? AND owner_id = ? AND purpose = ?",
            )
            .bind(&owner_module)
            .bind(&owner_type)
            .bind(&owner_id)
            .bind(&purpose)
            .execute(&self.pool)
            .await
            {
                Ok(_) => {
                    sqlx::query("DELETE FROM blob_cleanup_intents WHERE id = ?")
                        .bind(&id)
                        .execute(&self.pool)
                        .await?;
                    recovered += 1;
                }
                Err(error) => {
                    let attempts: i64 = sqlx::query_scalar(
                        "SELECT attempts FROM blob_cleanup_intents WHERE id = ?",
                    )
                    .bind(&id)
                    .fetch_one(&self.pool)
                    .await?;
                    let next_attempt = format_utc(
                        now_utc() + chrono::Duration::seconds((1_i64 << attempts.min(6)).min(60)),
                    );
                    sqlx::query(
                        "UPDATE blob_cleanup_intents SET attempts = attempts + 1, next_attempt_at = ?, last_error = ?, updated_at = ? WHERE id = ?",
                    )
                    .bind(next_attempt)
                    .bind(error.to_string())
                    .bind(now_utc_str())
                    .bind(&id)
                    .execute(&self.pool)
                    .await?;
                    return Err(anyhow::anyhow!(
                        "blob cleanup intent {id} failed: {error}"
                    ));
                }
            }
        }
        Ok(recovered)
    }

    /// Mark and sweep unreferenced CAS objects. Marking `trash` is durable so
    /// a crash during physical deletion is retried on the next startup. The
    /// shared storage lock prevents a concurrent write from racing the file
    /// removal, and the final reference check protects a newly-created root.
    pub async fn sweep_unreferenced(&self) -> anyhow::Result<usize> {
        let _storage_lock = self.storage_lock.lock().await;
        let rows: Vec<(String, String)> = sqlx::query_as(
            "SELECT object.sha256, object.storage_state FROM blob_objects AS object \
             WHERE object.storage_state IN ('present', 'trash') \
               AND NOT EXISTS (SELECT 1 FROM blob_references AS reference \
                               WHERE reference.blob_sha = object.sha256) \
             ORDER BY object.first_written_at, object.sha256",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut swept = 0;
        for (sha, state) in rows {
            let has_reference: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM blob_references WHERE blob_sha = ?)",
            )
            .bind(&sha)
            .fetch_one(&self.pool)
            .await?;
            if has_reference {
                sqlx::query("UPDATE blob_objects SET storage_state = 'present' WHERE sha256 = ?")
                    .bind(&sha)
                    .execute(&self.pool)
                    .await?;
                continue;
            }
            if state == "present" {
                sqlx::query(
                    "UPDATE blob_objects SET storage_state = 'trash' WHERE sha256 = ? AND storage_state = 'present'",
                )
                .bind(&sha)
                .execute(&self.pool)
                .await?;
            }
            let path = object_path(&self.objects_root, &sha);
            match tokio::fs::remove_file(&path).await {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    anyhow::bail!("sweep blob {}: {}", sha, error);
                }
            }
            let deleted = sqlx::query(
                "DELETE FROM blob_objects WHERE sha256 = ? AND storage_state = 'trash' \
                 AND NOT EXISTS (SELECT 1 FROM blob_references WHERE blob_sha = ?)",
            )
            .bind(&sha)
            .bind(&sha)
            .execute(&self.pool)
            .await?
            .rows_affected();
            if deleted > 0 {
                swept += 1;
            } else {
                sqlx::query("UPDATE blob_objects SET storage_state = 'present' WHERE sha256 = ?")
                    .bind(&sha)
                    .execute(&self.pool)
                    .await?;
            }
        }
        Ok(swept)
    }

    async fn enqueue_cleanup(&self, reference: &BlobReference, error: &str) -> anyhow::Result<()> {
        let now = now_utc_str();
        sqlx::query(
            "INSERT INTO blob_cleanup_intents \
             (id, owner_module, owner_type, owner_id, purpose, next_attempt_at, last_error, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT(owner_module, owner_type, owner_id, purpose) DO UPDATE SET \
             next_attempt_at = excluded.next_attempt_at, last_error = excluded.last_error, updated_at = excluded.updated_at",
        )
        .bind(format!("blob_cleanup_{}", uuid::Uuid::now_v7()))
        .bind(reference.owner_module)
        .bind(reference.owner_type)
        .bind(&reference.owner_id)
        .bind(reference.purpose)
        .bind(&now)
        .bind(error)
        .bind(&now)
        .bind(&now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Remove temporary files left by interrupted writes. Committed objects are
    /// retained here because reference collection needs domain-level ownership.
    pub async fn clean_incoming(&self) -> anyhow::Result<()> {
        let mut entries = match tokio::fs::read_dir(&self.incoming_root).await {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        while let Some(entry) = entries.next_entry().await? {
            // A successful write renames the temp out before committing its DB
            // reference, so a temp left here is safe to treat as crash debris.
            tokio::fs::remove_file(entry.path()).await?;
        }
        Ok(())
    }
}

/// A logical owner of blob bytes. Separate owners may share one content object;
/// each reference remains an independent GC root edge.
#[derive(Debug, Clone)]
pub struct BlobReference {
    pub owner_module: &'static str,
    pub owner_type: &'static str,
    pub owner_id: String,
    pub purpose: &'static str,
}

impl BlobReference {
    pub fn new(
        owner_module: &'static str,
        owner_type: &'static str,
        owner_id: &str,
        purpose: &'static str,
    ) -> Self {
        Self {
            owner_module,
            owner_type,
            owner_id: owner_id.to_owned(),
            purpose,
        }
    }
}

fn hex_sha256(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex_encode(hasher.finalize())
}
