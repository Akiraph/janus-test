use std::time::Duration;

use mongodb::{ClientSession, Database};

use super::events::{EventEnvelope, EventStore, NewEvent};

/// Writer serialization for the shared `event_seq` cursor counter.
///
/// The cursor is allocated and stored inside the same transaction as the event
/// (a rollback must not leave a gap in the log). The append mutex is therefore
/// held from the transaction start, not lazily on the first event append:
/// MongoDB snapshot isolation fails the `$inc` with a write conflict whenever
/// a transaction that committed after our snapshot has already bumped the
/// counter, so the lock must be acquired BEFORE `start_transaction`. Holding it
/// across the whole transaction gives single-writer serialization of the
/// counter. Standalone appends (`EventStore::append`) take the same lock, so
/// no path may
/// call that method from inside an open `UnitOfWorkTransaction`, or it would
/// self-deadlock.
#[derive(Clone)]
pub struct UnitOfWork {
    pool: Database,
    events: EventStore,
}

impl UnitOfWork {
    pub fn new(pool: Database, events: EventStore) -> Self {
        Self { pool, events }
    }

    pub fn pool(&self) -> &Database {
        &self.pool
    }

    pub async fn begin(&self) -> Result<UnitOfWorkTransaction, mongodb::error::Error> {
        for attempt in 0_u32..=4 {
            match self.events.begin_append_tx().await {
                Ok((guard, session)) => {
                    return Ok(UnitOfWorkTransaction {
                        session,
                        events: self.events.clone(),
                        event_count: 0,
                        _append_serial: guard,
                    });
                }
                Err(error)
                    if attempt < 4
                        && error.contains_label(mongodb::error::TRANSIENT_TRANSACTION_ERROR) =>
                {
                    let delay = Duration::from_millis(25 * (1_u64 << attempt));
                    tokio::time::sleep(delay).await;
                }
                Err(error) => return Err(error),
            }
        }
        Err(mongodb::error::Error::custom(
            "transaction start did not settle",
        ))
    }
}

pub struct UnitOfWorkTransaction {
    session: ClientSession,
    events: EventStore,
    event_count: usize,
    /// Held from transaction start (acquired before `start_transaction` in
    /// `begin`) until drop/commit/rollback, so the event-cursor `$inc` can
    /// never conflict with a transaction that committed earlier. `OwnedMutexGuard`
    /// owns the mutex Arc, so holding it next to the transaction avoids a
    /// self-referential borrow.
    _append_serial: tokio::sync::OwnedMutexGuard<()>,
}

impl UnitOfWorkTransaction {
    pub fn connection(&mut self) -> &mut ClientSession {
        &mut self.session
    }

    pub async fn append_event(&mut self, event: NewEvent) -> anyhow::Result<EventEnvelope> {
        let events = self.events.clone();
        let envelope = events.append_in_tx(&mut self.session, event).await?;
        self.event_count += 1;
        Ok(envelope)
    }

    pub async fn commit(mut self) -> Result<(), mongodb::error::Error> {
        // `UnknownTransactionCommitResult` means the server may or may not have
        // committed, so only the commit itself is retried, never the body.
        let mut attempts = 0_u32;
        loop {
            match self.session.commit_transaction().await {
                Ok(()) => {
                    if self.event_count > 0 {
                        self.events.notify_committed();
                    }
                    return Ok(());
                }
                Err(error)
                    if error.contains_label(mongodb::error::UNKNOWN_TRANSACTION_COMMIT_RESULT)
                        && attempts < 3 =>
                {
                    attempts += 1;
                }
                Err(error) => return Err(error),
            }
        }
    }

    pub async fn rollback(mut self) -> Result<(), mongodb::error::Error> {
        self.session.abort_transaction().await
    }
}
