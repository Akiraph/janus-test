use std::time::Duration;

use sqlx::{Sqlite, SqliteConnection, SqlitePool, Transaction};

use super::events::{EventEnvelope, EventStore, NewEvent};

#[derive(Clone)]
pub struct UnitOfWork {
    pool: SqlitePool,
    events: EventStore,
}

impl UnitOfWork {
    pub fn new(pool: SqlitePool, events: EventStore) -> Self {
        Self { pool, events }
    }

    pub async fn begin(&self) -> Result<UnitOfWorkTransaction<'_>, sqlx::Error> {
        let transaction = 'begin: {
            for attempt in 0_u32..=4 {
                match self.pool.begin_with("BEGIN IMMEDIATE").await {
                    Ok(transaction) => break 'begin transaction,
                    Err(error) if attempt < 4 && is_sqlite_busy(&error) => {
                        let delay = Duration::from_millis(25 * (1_u64 << attempt));
                        tokio::time::sleep(delay).await;
                    }
                    Err(error) => return Err(error),
                }
            }
            unreachable!("the transaction retry loop always returns or breaks")
        };
        Ok(UnitOfWorkTransaction {
            transaction,
            events: self.events.clone(),
            event_count: 0,
        })
    }
}

fn is_sqlite_busy(error: &sqlx::Error) -> bool {
    let sqlx::Error::Database(database) = error else {
        return false;
    };
    matches!(database.code().as_deref(), Some("5") | Some("6"))
        || database.message().contains("database is locked")
}

pub struct UnitOfWorkTransaction<'a> {
    transaction: Transaction<'a, Sqlite>,
    events: EventStore,
    event_count: usize,
}

impl UnitOfWorkTransaction<'_> {
    pub fn connection(&mut self) -> &mut SqliteConnection {
        &mut self.transaction
    }

    pub async fn append_event(&mut self, event: NewEvent) -> anyhow::Result<EventEnvelope> {
        let events = self.events.clone();
        let envelope = events.append_in_tx(self.connection(), event).await?;
        self.event_count += 1;
        Ok(envelope)
    }

    pub async fn commit(self) -> Result<(), sqlx::Error> {
        let Self {
            transaction,
            events,
            event_count,
        } = self;
        transaction.commit().await?;
        if event_count > 0 {
            events.notify_committed();
        }
        Ok(())
    }

    pub async fn rollback(self) -> Result<(), sqlx::Error> {
        self.transaction.rollback().await
    }
}
