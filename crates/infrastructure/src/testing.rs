//! Shared test harness for MongoDB-backed unit tests.
//!
//! Enabled via the `testing` feature; every test gets its own uniquely named
//! throwaway database so parallel test runs never collide. Requires a running
//! MongoDB single-node replica set (for transactions), which CI starts as a
//! service container.

use std::sync::Arc;

use crate::database::Database;

/// A configured throwaway database, torn down best-effort when the last handle
/// goes away. Clone the `Arc` to share one database within a test.
pub struct TestDb {
    client: mongodb::Client,
    database: mongodb::Database,
    /// Unique database name; keeps concurrent tests from colliding.
    pub name: String,
    _data_root: tempfile::TempDir,
}

impl TestDb {
    /// Connect to `JANUS_MONGODB_URI` (default `mongodb://localhost:27017`) and
    /// open a uniquely named database with the full Janus collection catalog.
    pub async fn open() -> anyhow::Result<Arc<Self>> {
        let uri = std::env::var("JANUS_MONGODB_URI")
            .unwrap_or_else(|_| "mongodb://localhost:27017".to_owned());
        let name = format!("janus_test_{}", uuid::Uuid::now_v7().simple());
        let data_root = tempfile::tempdir()?;
        let database = Database::open(data_root.path(), &uri, &name).await?;
        Ok(Arc::new(Self {
            client: database.client().clone(),
            database: database.pool().clone(),
            name,
            _data_root: data_root,
        }))
    }

    pub fn client(&self) -> &mongodb::Client {
        &self.client
    }

    pub fn database(&self) -> &mongodb::Database {
        &self.database
    }

    pub fn events(&self) -> crate::events::EventStore {
        crate::events::EventStore::new(self.database.clone())
    }

    pub fn unit_of_work(&self) -> crate::unit_of_work::UnitOfWork {
        crate::unit_of_work::UnitOfWork::new(self.database.clone(), self.events())
    }

    pub fn operations(&self) -> crate::operations::OperationInterface {
        crate::operations::OperationInterface::new(self.database.clone(), self.events())
    }

    pub fn blobs(&self) -> anyhow::Result<crate::managed_storage::BlobStore> {
        crate::managed_storage::BlobStore::new(self.database.clone(), self._data_root.path())
    }
}

impl Drop for TestDb {
    fn drop(&mut self) {
        // Best-effort teardown from a synchronous context. A leaked database in
        // a throwaway CI replica set is harmless; unique names keep runs isolated.
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            let database = self.database.clone();
            handle.spawn(async move {
                let _ = database.drop().await;
            });
        }
    }
}
