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
        Ok(UnitOfWorkTransaction {
            transaction: self.pool.begin_with("BEGIN IMMEDIATE").await?,
            events: self.events.clone(),
            event_count: 0,
        })
    }
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
