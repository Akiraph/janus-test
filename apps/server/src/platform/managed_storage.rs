//! Content-addressed object store (SHA-256 CAS).
//!
//! M2 scope (see `DAT-BLOB-01/02`): atomic writes, length verification on
//! hash-collision rewrite, reference registration, and incoming temp cleanup.
//! Full mark-and-sweep GC and Merkle manifest collection arrive in M3/M8; M2
//! only guarantees referenced objects are never collected and that unreferenced
//! incoming temps are cleaned on startup.

use std::path::{Path, PathBuf};

use anyhow::Context;
use sha2::{Digest, Sha256};
use sqlx::SqlitePool;
use tokio::io::AsyncWriteExt;

use super::{
    clock::{Clock, SystemClock, format_utc},
    id::BlobSha,
};
use rand::RngCore;

/// Two-char directory shard under `objects/sha256/`, matching `DAT` layout.
fn shard(sha: &str) -> String {
    sha.chars().take(2).collect()
}

/// Full object path for a SHA-256 hex digest.
fn object_path(objects_root: &Path, sha: &str) -> PathBuf {
    objects_root.join("sha256").join(shard(sha)).join(sha)
}

#[derive(Clone)]
pub struct BlobStore {
    pool: SqlitePool,
    objects_root: PathBuf,
    incoming_root: PathBuf,
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
        })
    }

    /// Write `bytes` to the CAS and register the object + a reference.
    ///
    /// Flow (`DAT-BLOB-01`): random incoming temp -> stream SHA-256 -> fsync ->
    /// same-filesystem atomic rename -> fsync parent dir -> DB transaction
    /// registering object and reference. An existing object with the same hash
    /// is length-verified before reuse; we never trust the filename alone.
    pub async fn write(&self, bytes: &[u8], reference: BlobReference) -> anyhow::Result<BlobSha> {
        let sha = hex_sha256(bytes);
        let target = object_path(&self.objects_root, &sha);

        // Idempotent fast path: object already present -> verify length, register ref.
        if tokio::fs::try_exists(&target).await.unwrap_or(false) {
            self.verify_length(&sha, bytes.len()).await?;
            self.register_reference(&sha, bytes.len(), reference)
                .await?;
            return Ok(BlobSha::from_hex(sha));
        }

        // Slow path: write to incoming, fsync, atomic rename, fsync parent.
        let incoming = self.incoming_root.join(format!("{}.tmp", random_name()));
        {
            let mut file = tokio::fs::File::create(&incoming)
                .await
                .with_context(|| format!("create incoming temp {}", incoming.display()))?;
            file.write_all(bytes).await?;
            file.sync_all().await?;
        }
        // Ensure the two-char shard directory exists, then atomic rename.
        if let Some(parent) = target.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .with_context(|| format!("create shard dir {}", parent.display()))?;
        }
        tokio::fs::rename(&incoming, &target)
            .await
            .with_context(|| format!("atomic rename to {}", target.display()))?;
        if let Some(parent) = target.parent() {
            // Best-effort fsync of the parent so the rename survives power loss.
            // On Windows, opening a directory as a File often returns Access Denied;
            // treat that as non-fatal (object bytes are already renamed into place).
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
                // Object file exists but no row yet: register it from the file.
                let path = object_path(&self.objects_root, sha);
                let meta = tokio::fs::metadata(&path).await?;
                let size = i64::try_from(meta.len())?;
                let now = format_utc(SystemClock.now());
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
        let now = format_utc(SystemClock.now());
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
        let changed = sqlx::query("DELETE FROM blob_references WHERE owner_module = ? AND owner_type = ? AND owner_id = ? AND purpose = ?")
            .bind(reference.owner_module)
            .bind(reference.owner_type)
            .bind(reference.owner_id.as_str())
            .bind(reference.purpose)
            .execute(&self.pool)
            .await?
            .rows_affected();
        Ok(changed > 0)
    }

    /// Clean unreferenced leftover files in `incoming/`. Called during startup
    /// recovery (`DAT-RECOVER-01`). Objects in `objects/sha256` are only ever
    /// removed by mark-and-sweep GC (M8), never here.
    pub async fn clean_incoming(&self) -> anyhow::Result<()> {
        let mut entries = match tokio::fs::read_dir(&self.incoming_root).await {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        while let Some(entry) = entries.next_entry().await? {
            // Incoming temps are safe to remove: a real object write renames them
            // out before committing the DB transaction, so anything left here is
            // from a crashed write.
            let _ = tokio::fs::remove_file(entry.path()).await;
        }
        Ok(())
    }
}

/// A logical owner of blob bytes. Used as the GC root graph edge; two distinct
/// owners can share the same bytes via separate references.
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
    hex::encode(hasher.finalize())
}

fn random_name() -> String {
    let mut bytes = [0u8; 16];
    rand::rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}
