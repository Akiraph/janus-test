use std::{fs::OpenOptions, path::Path};

use anyhow::{Context, bail};
use fs2::FileExt;
use mongodb::{
    bson::{doc, Document},
    options::UpdateOptions,
};

use crate::schema::{COLLECTIONS, INDEXLESS_COLLECTIONS, SCHEMA_VERSION, index_specs};

pub struct Database {
    client: mongodb::Client,
    database: mongodb::Database,
    _data_root_lock: std::fs::File,
}

impl Database {
    pub async fn open(
        data_root: &Path,
        uri: &str,
        database_name: &str,
    ) -> anyhow::Result<Self> {
        std::fs::create_dir_all(data_root)
            .with_context(|| format!("create data root {}", data_root.display()))?;
        let lock_path = data_root.join("janus.lock");
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_path)
            .with_context(|| format!("open data-root lock {}", lock_path.display()))?;
        if lock.try_lock_exclusive().is_err() {
            bail!("data root is already in use: {}", data_root.display());
        }

        let client = mongodb::Client::with_uri_str(uri)
            .await
            .with_context(|| format!("connect to MongoDB at {uri}"))?;
        let database = client.database(database_name);

        // `create_indexes` implicitly materializes an indexed collection, but an
        // index-less collection never would, so create those explicitly first.
        for name in INDEXLESS_COLLECTIONS {
            let exists = database
                .list_collection_names()
                .await?
                .iter()
                .any(|existing| existing == name);
            if !exists {
                database
                    .create_collection(name)
                    .await
                    .with_context(|| format!("create collection {name}"))?;
            }
        }
        for (name, models) in index_specs() {
            database
                .collection::<Document>(name)
                .create_indexes(models)
                .await
                .with_context(|| format!("create indexes on {name}"))?;
        }
        // Seed the event cursor counter so the first append is deterministic.
        database
            .collection::<Document>("event_seq")
            .update_one(
                doc! {"_id": "global"},
                doc! {"$setOnInsert": {"value": 0i64}},
                UpdateOptions::builder().upsert(true).build(),
            )
            .await
            .context("seed event cursor counter")?;

        Ok(Self {
            client,
            database,
            _data_root_lock: lock,
        })
    }

    pub fn client(&self) -> &mongodb::Client {
        &self.client
    }

    pub const fn pool(&self) -> &mongodb::Database {
        &self.database
    }

    /// Convenience handle for a collection; collection names are inline
    /// literals at every call site so the xtask ownership pass can find them.
    pub fn collection(&self, name: &str) -> mongodb::Collection<Document> {
        self.database.collection::<Document>(name)
    }

    /// All collections this schema knows about, for startup sanity checks.
    pub fn known_collections() -> Vec<String> {
        COLLECTIONS
            .iter()
            .map(|(name, _)| name.to_string())
            .collect()
    }

    pub async fn schema_version(&self) -> anyhow::Result<i64> {
        Ok(SCHEMA_VERSION)
    }

    pub async fn ready(&self) -> bool {
        self.database
            .run_command(doc! {"ping": 1})
            .await
            .is_ok()
    }
}
