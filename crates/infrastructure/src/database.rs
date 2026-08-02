use std::{fs::OpenOptions, path::Path, str::FromStr, time::Duration};

use anyhow::{Context, bail};
use fs2::FileExt;
use sqlx::{
    SqlitePool,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions},
};

pub struct Database {
    pool: SqlitePool,
    _data_root_lock: std::fs::File,
}

impl Database {
    pub async fn open(
        data_root: &Path,
        migrator: &sqlx::migrate::Migrator,
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

        let database_path = data_root.join("janus.db");
        let options = SqliteConnectOptions::from_str(&database_path.to_string_lossy())?
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .foreign_keys(true)
            .busy_timeout(Duration::from_secs(5));
        let pool = SqlitePoolOptions::new()
            .max_connections(8)
            .connect_with(options)
            .await
            .with_context(|| format!("open SQLite database {}", database_path.display()))?;
        migrator
            .run(&pool)
            .await
            .context("run global database migrations")?;

        Ok(Self {
            pool,
            _data_root_lock: lock,
        })
    }

    pub const fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    pub async fn schema_version(&self) -> anyhow::Result<i64> {
        let version = sqlx::query_scalar::<_, Option<i64>>(
            "SELECT MAX(version) FROM _sqlx_migrations WHERE success = 1",
        )
        .fetch_one(&self.pool)
        .await?;
        Ok(version.unwrap_or(0))
    }

    pub async fn ready(&self) -> bool {
        sqlx::query_scalar::<_, i64>("SELECT 1")
            .fetch_one(&self.pool)
            .await
            .is_ok()
    }
}
