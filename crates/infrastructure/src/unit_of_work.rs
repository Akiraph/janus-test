use std::time::Duration;

use mongodb::{ClientSession, Database};

use super::events::{EventEnvelope, EventStore, NewEvent};

/// Writer serialization for the shared `event_seq` cursor counter.
///
/// The cursor is allocated and stored inside the same transaction as the event
/// (a rollback must not leave a gap in the log). Two overlapping transactions
/// that both `$inc` the same `event_seq` document conflict at the server, so the
/// append mutex is held from the first event append until commit — the MongoDB
/// analogue of SQLite's `BEGIN IMMEDIATE` single-writer semantics, acquired
/// lazily so a transaction that only writes non-event state never contends.
/// Standalone appends (`EventStore::append`) take the same lock, so no path may
/// call that method from inside an open `UnitOfWorkTransaction` that has already
/// appended, or it would self-deadlock.
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
            let mut session = self.pool.client().start_session().await?;
            match session.start_transaction().await {
                Ok(()) => {
                    return Ok(UnitOfWorkTransaction {
                        session,
                        events: self.events.clone(),
                        event_count: 0,
                        _append_serial: None,
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
    /// Acquired on the first event append, so a transaction that only writes
    /// non-event state never contends on the process-wide event-cursor lock.
    /// Released on drop/commit/rollback. `OwnedMutexGuard` owns the mutex Arc,
    /// so holding it next to the transaction avoids a self-referential borrow.
    _append_serial: Option<tokio::sync::OwnedMutexGuard<()>>,
}

impl UnitOfWorkTransaction {
    pub fn connection(&mut self) -> &mut ClientSession {
        &mut self.session
    }

    pub async fn append_event(&mut self, event: NewEvent) -> anyhow::Result<EventEnvelope> {
        if self._append_serial.is_none() {
            self._append_serial = Some(self.events.append_lock().await);
        }
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
