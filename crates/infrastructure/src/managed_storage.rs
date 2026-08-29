//! Content-addressed blob store (SHA-256 CAS).
//!
//! Registers references for GC roots and performs conservative mark-and-sweep
//! collection. Only objects with no logical reference can reach `trash`.

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::Arc,
};

use crate::clock::{format_utc, now_utc, now_utc_str};
use anyhow::Context;
use futures_util::TryStreamExt;
use mongodb::{
    bson::{Document, doc},
};
use sha2::{Digest, Sha256};
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
    pool: mongodb::Database,
    objects_root: PathBuf,
    incoming_root: PathBuf,
    storage_lock: Arc<tokio::sync::Mutex<()>>,
}

impl BlobStore {
    pub fn new(pool: mongodb::Database, data_root: &Path) -> anyhow::Result<Self> {
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
        let document = self
            .pool
            .collection::<Document>("blob_objects")
            .find_one(doc! {"_id": sha})
            .await?;
        match document {
            Some(document) => {
                let recorded = document.get_i64("byte_size")?;
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
                self.pool
                    .collection::<Document>("blob_objects")
                    .update_one(
                        doc! {"_id": sha},
                        doc! {
                            "$setOnInsert": {
                                "byte_size": size,
                                "storage_state": "present",
                                "first_written_at": &now,
                            }
                        },
                    )
                    .upsert(true)
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
        let size = i64::try_from(size)?;
        let now = now_utc_str();
        let mut session = self.pool.client().start_session().await?;
        session.start_transaction().await?;
        self.pool
            .collection::<Document>("blob_objects")
            .update_one(
                doc! {"_id": sha},
                doc! {
                    "$setOnInsert": {
                        "byte_size": size,
                        "storage_state": "present",
                        "first_written_at": &now,
                    }
                },
            )
            .upsert(true)
            .session(&mut session)
            .await?;
        self.pool
            .collection::<Document>("blob_references")
            .update_one(
                doc! {
                    "owner_module": reference.owner_module,
                    "owner_type": reference.owner_type,
                    "owner_id": reference.owner_id.as_str(),
                    "purpose": reference.purpose,
                },
                doc! {"$setOnInsert": {"blob_sha": sha, "created_at": &now}},
            )
            .upsert(true)
            .session(&mut session)
            .await?;
        session.commit_transaction().await?;
        Ok(())
    }

    /// Remove a single logical reference. Returns true if the reference existed.
    pub async fn drop_reference(&self, reference: &BlobReference) -> anyhow::Result<bool> {
        let result = self
            .pool
            .collection::<Document>("blob_references")
            .delete_one(doc! {
                "owner_module": reference.owner_module,
                "owner_type": reference.owner_type,
                "owner_id": reference.owner_id.as_str(),
                "purpose": reference.purpose,
            })
            .await;
        match result {
            Ok(result) => Ok(result.deleted_count > 0),
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
        let mut cursor = self
            .pool
            .collection::<Document>("blob_cleanup_intents")
            .find(doc! {"next_attempt_at": {"$lte": &now}})
            .sort(doc! {"next_attempt_at": 1, "updated_at": 1, "_id": 1})
            .await?;
        let mut recovered = 0;
        while let Some(document) = cursor.try_next().await? {
            let id = document.get_str("_id")?.to_owned();
            let owner_module = document.get_str("owner_module")?;
            let owner_type = document.get_str("owner_type")?;
            let owner_id = document.get_str("owner_id")?;
            let purpose = document.get_str("purpose")?;
            match self
                .pool
                .collection::<Document>("blob_references")
                .delete_one(doc! {
                    "owner_module": owner_module,
                    "owner_type": owner_type,
                    "owner_id": owner_id,
                    "purpose": purpose,
                })
                .await
            {
                Ok(_) => {
                    self.pool
                        .collection::<Document>("blob_cleanup_intents")
                        .delete_one(doc! {"_id": &id})
                        .await?;
                    recovered += 1;
                }
                Err(error) => {
                    let attempts = document.get_i64("attempts").unwrap_or(0);
                    let next_attempt = format_utc(
                        now_utc() + chrono::Duration::seconds((1_i64 << attempts.min(6)).min(60)),
                    );
                    self.pool
                        .collection::<Document>("blob_cleanup_intents")
                        .update_one(
                            doc! {"_id": &id},
                            doc! {
                                "$inc": {"attempts": 1i64},
                                "$set": {
                                    "next_attempt_at": next_attempt,
                                    "last_error": error.to_string(),
                                    "updated_at": now_utc_str(),
                                }
                            },
                        )
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
        let mut objects = self
            .pool
            .collection::<Document>("blob_objects")
            .find(doc! {"storage_state": {"$in": ["present", "trash"]}})
            .sort(doc! {"first_written_at": 1, "_id": 1})
            .await?;
        let mut candidates = Vec::new();
        while let Some(document) = objects.try_next().await? {
            candidates.push(document);
        }
        // Referenced set replaces the SQL `NOT EXISTS` correlation.
        let mut references = self
            .pool
            .collection::<Document>("blob_references")
            .find(doc! {})
            .await?;
        let mut referenced = HashSet::new();
        while let Some(document) = references.try_next().await? {
            if let Some(sha) = document.get_str("blob_sha").ok() {
                referenced.insert(sha.to_owned());
            }
        }
        let mut swept = 0;
        for document in candidates {
            let sha = document.get_str("_id")?.to_owned();
            let state = document.get_str("storage_state")?.to_owned();
            // A reference may have landed after the snapshot was taken.
            if referenced.contains(&sha) {
                self.pool
                    .collection::<Document>("blob_objects")
                    .update_one(doc! {"_id": &sha}, doc! {"$set": {"storage_state": "present"}})
                    .await?;
                continue;
            }
            if state == "present" {
                self.pool
                    .collection::<Document>("blob_objects")
                    .update_one(
                        doc! {"_id": &sha, "storage_state": "present"},
                        doc! {"$set": {"storage_state": "trash"}},
                    )
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
            // Final fence against a newly-registered root between snapshot and file removal.
            let still_referenced = self
                .pool
                .collection::<Document>("blob_references")
                .find_one(doc! {"blob_sha": &sha})
                .await?;
            if still_referenced.is_some() {
                self.pool
                    .collection::<Document>("blob_objects")
                    .update_one(doc! {"_id": &sha}, doc! {"$set": {"storage_state": "present"}})
                    .await?;
                continue;
            }
            let deleted = self
                .pool
                .collection::<Document>("blob_objects")
                .delete_one(doc! {"_id": &sha, "storage_state": "trash"})
                .await?
                .deleted_count;
            if deleted > 0 {
                swept += 1;
            } else {
                self.pool
                    .collection::<Document>("blob_objects")
                    .update_one(doc! {"_id": &sha}, doc! {"$set": {"storage_state": "present"}})
                    .await?;
            }
        }
        Ok(swept)
    }

    async fn enqueue_cleanup(&self, reference: &BlobReference, error: &str) -> anyhow::Result<()> {
        let now = now_utc_str();
        self.pool
            .collection::<Document>("blob_cleanup_intents")
            .update_one(
                doc! {
                    "owner_module": reference.owner_module,
                    "owner_type": reference.owner_type,
                    "owner_id": reference.owner_id.as_str(),
                    "purpose": reference.purpose,
                },
                doc! {
                    "$set": {
                        "next_attempt_at": &now,
                        "last_error": error,
                        "updated_at": &now,
                    },
                    "$setOnInsert": {
                        "_id": format!("blob_cleanup_{}", uuid::Uuid::now_v7()),
                        "attempts": 0i64,
                        "created_at": &now,
                    }
                },
            )
            .upsert(true)
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
