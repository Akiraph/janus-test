//! Content-addressed blob store (SHA-256 CAS).
//!
//! Registers references for GC roots; mark-and-sweep collection lives elsewhere.
//! Only unreferenced crash leftovers in `incoming/` are removed here.

use std::path::{Path, PathBuf};

use crate::clock::now_utc_str;
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

    /// Write bytes to the content-addressed store and register one logical reference.
    ///
    /// The file becomes durable before the reference transaction commits. Keeping
    /// the temporary and final paths on one filesystem makes the rename atomic;
    /// a crash can therefore leave recoverable debris but not a dangling reference.
    pub async fn write(&self, bytes: &[u8], reference: BlobReference) -> anyhow::Result<BlobSha> {
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
            let _ = tokio::fs::remove_file(entry.path()).await;
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
