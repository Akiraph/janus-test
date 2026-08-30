use std::{path::Path, sync::Arc};

use janus_infrastructure::clock::now_utc_str;
use mongodb::bson::{Document, doc};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use super::interface::{
    LogChannel, LogChunk, LogCursor, LogOwnerKind, LogRange, LogStreamProjection, RuntimeError,
};
use janus_infrastructure::id::LogStreamId;

const CHUNK_BYTES: usize = 64 * 1024;
const MAX_READ_BYTES: usize = 1024 * 1024;
static TEMP_FILE_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

#[derive(Debug, Clone, Copy)]
pub struct LogRetention {
    pub raw_limit_bytes: u64,
    pub head_bytes: u64,
}

impl LogRetention {
    pub const ASYNC_TASK: Self = Self {
        raw_limit_bytes: 512 * 1024 * 1024,
        head_bytes: 1024 * 1024,
    };
    pub const TERMINAL: Self = Self {
        raw_limit_bytes: 16 * 1024 * 1024,
        head_bytes: 1024 * 1024,
    };

    fn validate(self) -> Result<(), RuntimeError> {
        if self.raw_limit_bytes == 0 || self.head_bytes >= self.raw_limit_bytes {
            return Err(RuntimeError::InvalidSpec(
                "log retention requires a nonzero limit larger than the retained head".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Clone)]
pub struct LogStore {
    pool: mongodb::Database,
    root: Arc<std::path::PathBuf>,
    gate: Arc<Mutex<()>>,
}

struct StreamRow {
    id: String,
    relative_path: String,
    first_cursor: i64,
    next_cursor: i64,
    retained_bytes: i64,
    total_bytes: i64,
    truncated: bool,
    closed: bool,
}

impl StreamRow {
    fn from_document(document: &Document) -> Result<Self, RuntimeError> {
        Ok(Self {
            id: document.get_str("_id").map_err(storage_error)?.to_owned(),
            relative_path: document
                .get_str("relative_path")
                .map_err(storage_error)?
                .to_owned(),
            first_cursor: document.get_i64("first_cursor").map_err(storage_error)?,
            next_cursor: document.get_i64("next_cursor").map_err(storage_error)?,
            retained_bytes: document.get_i64("retained_bytes").map_err(storage_error)?,
            total_bytes: document.get_i64("total_bytes").map_err(storage_error)?,
            truncated: document.get_bool("truncated").map_err(storage_error)?,
            closed: document.get_bool("closed").map_err(storage_error)?,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DiskChunk {
    start: u64,
    end: u64,
    channel: LogChannel,
    text: String,
    marker: bool,
}

impl LogStore {
    pub fn new(pool: mongodb::Database, data_root: &Path) -> Self {
        Self {
            pool,
            root: Arc::new(data_root.join("runtime").join("logs")),
            gate: Arc::new(Mutex::new(())),
        }
    }

    pub async fn create(
        &self,
        owner: LogOwnerKind,
        owner_id: &str,
    ) -> Result<LogStreamProjection, RuntimeError> {
        let _guard = self.gate.lock().await;
        let id = LogStreamId::new();
        let id_string = id.to_string();
        let relative_path = id_string.clone();
        tokio::fs::create_dir_all(self.root.join(&relative_path))
            .await
            .map_err(storage_error)?;
        let now = now_utc_str();
        // Sync streams are per command invocation. The runtime id is reused
        // for every synchronous command, so it cannot satisfy the historical
        // owner uniqueness constraint.
        let stored_owner_id = if owner == LogOwnerKind::Sync {
            id_string.clone()
        } else {
            owner_id.to_owned()
        };
        self.pool
            .collection::<Document>("log_streams")
            .insert_one(doc! {
                "_id": &id_string,
                "owner_kind": owner.as_str(),
                "owner_id": &stored_owner_id,
                "relative_path": &relative_path,
                "first_cursor": 0i64,
                "next_cursor": 0i64,
                "retained_bytes": 0i64,
                "total_bytes": 0i64,
                "truncated": false,
                "closed": false,
                "created_at": &now,
                "updated_at": &now,
            })
            .await
            .map_err(storage_error)?;
        Ok(LogStreamProjection {
            id,
            first_cursor: LogCursor::new(0),
            next_cursor: LogCursor::new(0),
            retained_bytes: 0,
            total_bytes: 0,
            truncated: false,
            closed: false,
        })
    }

    pub async fn append(
        &self,
        id: LogStreamId,
        channel: LogChannel,
        input: &[u8],
        secret_values: &[&str],
        retention: LogRetention,
    ) -> Result<LogStreamProjection, RuntimeError> {
        retention.validate()?;
        if input.is_empty() {
            return self.projection(id).await;
        }
        let _guard = self.gate.lock().await;
        let row = self.row(id).await?;
        if row.closed {
            return Err(RuntimeError::ResourceBusy);
        }
        let text = redact(&String::from_utf8_lossy(input), secret_values);
        let mut cursor = to_u64(row.next_cursor, "next_cursor")?;
        let directory = self.root.join(&row.relative_path);
        for part in split_text(&text, CHUNK_BYTES) {
            let start = cursor;
            cursor = cursor.saturating_add(u64::try_from(part.len()).unwrap_or(u64::MAX));
            write_chunk(
                &directory,
                &DiskChunk {
                    start,
                    end: cursor,
                    channel,
                    text: part.to_owned(),
                    marker: false,
                },
            )
            .await?;
        }
        let total_bytes = to_u64(row.total_bytes, "total_bytes")?
            .saturating_add(u64::try_from(input.len()).unwrap_or(u64::MAX));
        let (first_cursor, retained_bytes, truncated) =
            enforce_retention(&directory, cursor, total_bytes, retention).await?;
        let now = now_utc_str();
        let first_cursor = to_i64(first_cursor)?;
        let next_cursor = to_i64(cursor)?;
        let retained_bytes = to_i64(retained_bytes)?;
        let total_bytes = to_i64(total_bytes)?;
        self.pool
            .collection::<Document>("log_streams")
            .update_one(
                doc! {"_id": id.to_string()},
                doc! {
                    "$set": {
                        "first_cursor": first_cursor,
                        "next_cursor": next_cursor,
                        "retained_bytes": retained_bytes,
                        "total_bytes": total_bytes,
                        "truncated": truncated,
                        "updated_at": &now,
                    }
                },
            )
            .await
            .map_err(storage_error)?;
        self.projection_unlocked(id).await
    }

    pub async fn close(&self, id: LogStreamId) -> Result<LogStreamProjection, RuntimeError> {
        let _guard = self.gate.lock().await;
        let now = now_utc_str();
        let changed = self
            .pool
            .collection::<Document>("log_streams")
            .update_one(
                doc! {"_id": id.to_string()},
                doc! {"$set": {"closed": true, "updated_at": &now}},
            )
            .await
            .map_err(storage_error)?;
        if changed.matched_count == 0 {
            return Err(RuntimeError::unavailable(format!(
                "log stream {id} does not exist"
            )));
        }
        self.projection_unlocked(id).await
    }

    pub(crate) async fn delete_files(&self, ids: &[LogStreamId]) -> Result<(), RuntimeError> {
        let _guard = self.gate.lock().await;
        for id in ids {
            match tokio::fs::remove_dir_all(self.root.join(id.to_string())).await {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(storage_error(error)),
            }
        }
        Ok(())
    }

    pub async fn projection(&self, id: LogStreamId) -> Result<LogStreamProjection, RuntimeError> {
        let _guard = self.gate.lock().await;
        self.projection_unlocked(id).await
    }

    pub async fn read(
        &self,
        id: LogStreamId,
        after: LogCursor,
        limit_bytes: usize,
    ) -> Result<LogRange, RuntimeError> {
        let _guard = self.gate.lock().await;
        let row = self.row(id).await?;
        let stream = projection_from_row(&row)?;
        if after.value() < stream.first_cursor.value() {
            return Err(RuntimeError::TerminalScrollbackExpired {
                first_cursor: stream.first_cursor,
            });
        }
        let limit = limit_bytes.clamp(1, MAX_READ_BYTES);
        let mut chunks = read_chunks(&self.root.join(&row.relative_path)).await?;
        chunks.sort_by_key(|chunk| (chunk.start, !chunk.marker));
        let mut remaining = limit;
        let mut result = Vec::new();
        for chunk in chunks.into_iter().filter(|chunk| chunk.end > after.value()) {
            if remaining == 0 {
                break;
            }
            let (start, source) = if chunk.marker {
                (chunk.start, chunk.text.as_str())
            } else {
                let requested = usize::try_from(after.value().saturating_sub(chunk.start))
                    .unwrap_or(usize::MAX)
                    .min(chunk.text.len());
                let offset = char_boundary_at_or_after(&chunk.text, requested);
                (
                    chunk
                        .start
                        .saturating_add(u64::try_from(offset).unwrap_or(u64::MAX)),
                    &chunk.text[offset..],
                )
            };
            let take = char_boundary_at_or_before(source, remaining);
            if take == 0 {
                break;
            }
            let text = source[..take].to_owned();
            let end = if take == source.len() || chunk.marker {
                chunk.end
            } else {
                start.saturating_add(u64::try_from(take).unwrap_or(u64::MAX))
            };
            result.push(LogChunk {
                start_cursor: LogCursor::new(start),
                end_cursor: LogCursor::new(end),
                channel: chunk.channel,
                text,
            });
            remaining = remaining.saturating_sub(take);
        }
        Ok(LogRange {
            stream,
            after,
            chunks: result,
        })
    }

    async fn projection_unlocked(
        &self,
        id: LogStreamId,
    ) -> Result<LogStreamProjection, RuntimeError> {
        projection_from_row(&self.row(id).await?)
    }

    async fn row(&self, id: LogStreamId) -> Result<StreamRow, RuntimeError> {
        match self
            .pool
            .collection::<Document>("log_streams")
            .find_one(doc! {"_id": id.to_string()})
            .await
            .map_err(storage_error)?
        {
            Some(document) => StreamRow::from_document(&document),
            None => Err(RuntimeError::unavailable(format!(
                "log stream {id} does not exist"
            ))),
        }
    }
}

fn projection_from_row(row: &StreamRow) -> Result<LogStreamProjection, RuntimeError> {
    Ok(LogStreamProjection {
        id: row.id.parse().map_err(|_| {
            RuntimeError::unavailable(format!("log stream id {:?} is invalid", row.id))
        })?,
        first_cursor: LogCursor::new(to_u64(row.first_cursor, "first_cursor")?),
        next_cursor: LogCursor::new(to_u64(row.next_cursor, "next_cursor")?),
        retained_bytes: to_u64(row.retained_bytes, "retained_bytes")?,
        total_bytes: to_u64(row.total_bytes, "total_bytes")?,
        truncated: row.truncated,
        closed: row.closed,
    })
}

async fn enforce_retention(
    directory: &Path,
    next_cursor: u64,
    total_bytes: u64,
    retention: LogRetention,
) -> Result<(u64, u64, bool), RuntimeError> {
    let mut chunks = read_chunks(directory).await?;
    if total_bytes <= retention.raw_limit_bytes {
        let retained = chunks
            .iter()
            .filter(|chunk| !chunk.marker)
            .fold(0_u64, |total, chunk| {
                total.saturating_add(u64::try_from(chunk.text.len()).unwrap_or(u64::MAX))
            });
        return Ok((
            chunks.first().map_or(0, |chunk| chunk.start),
            retained,
            false,
        ));
    }
    let tail_bytes = retention
        .raw_limit_bytes
        .saturating_sub(retention.head_bytes);
    let tail_start = next_cursor.saturating_sub(tail_bytes);
    let marker_path = directory.join("truncation-marker.json");
    let mut dropped_start = None::<u64>;
    let mut dropped_end = 0_u64;
    for chunk in chunks.iter().filter(|chunk| !chunk.marker) {
        let requested_start = chunk.start.max(retention.head_bytes);
        let requested_end = chunk.end.min(tail_start);
        if requested_start >= requested_end {
            continue;
        }
        let prefix_len = usize::try_from(requested_start.saturating_sub(chunk.start))
            .unwrap_or(usize::MAX)
            .min(chunk.text.len());
        let prefix_len = char_boundary_at_or_before(&chunk.text, prefix_len);
        let suffix_start = usize::try_from(requested_end.saturating_sub(chunk.start))
            .unwrap_or(usize::MAX)
            .min(chunk.text.len());
        let suffix_start = char_boundary_at_or_after(&chunk.text, suffix_start);
        let drop_start = chunk
            .start
            .saturating_add(u64::try_from(prefix_len).unwrap_or(u64::MAX));
        let drop_end = chunk
            .start
            .saturating_add(u64::try_from(suffix_start).unwrap_or(u64::MAX));
        dropped_start = Some(dropped_start.map_or(drop_start, |value| value.min(drop_start)));
        dropped_end = dropped_end.max(drop_end);
        let path = directory.join(chunk_file_name(chunk));
        match tokio::fs::remove_file(path).await {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(storage_error(error)),
        }
        if prefix_len != 0 {
            write_chunk(
                directory,
                &DiskChunk {
                    start: chunk.start,
                    end: drop_start,
                    channel: chunk.channel,
                    text: chunk.text[..prefix_len].to_owned(),
                    marker: false,
                },
            )
            .await?;
        }
        if suffix_start < chunk.text.len() {
            write_chunk(
                directory,
                &DiskChunk {
                    start: drop_end,
                    end: chunk.end,
                    channel: chunk.channel,
                    text: chunk.text[suffix_start..].to_owned(),
                    marker: false,
                },
            )
            .await?;
        }
    }
    if let Some(start) = dropped_start {
        let omitted = dropped_end.saturating_sub(start);
        let marker = DiskChunk {
            start,
            end: dropped_end,
            channel: LogChannel::System,
            text: format!("[Janus log truncated: {omitted} bytes omitted]\n"),
            marker: true,
        };
        write_atomic(
            &marker_path,
            &serde_json::to_vec(&marker).map_err(storage_error)?,
            true,
        )
        .await?;
    }
    chunks = read_chunks(directory).await?;
    let retained = chunks.iter().fold(0_u64, |total, chunk| {
        total.saturating_add(u64::try_from(chunk.text.len()).unwrap_or(u64::MAX))
    });
    let first = chunks
        .iter()
        .map(|chunk| chunk.start)
        .min()
        .unwrap_or(next_cursor);
    Ok((first, retained, true))
}

async fn write_chunk(directory: &Path, chunk: &DiskChunk) -> Result<(), RuntimeError> {
    let path = directory.join(chunk_file_name(chunk));
    let bytes = serde_json::to_vec(chunk).map_err(storage_error)?;
    write_atomic(&path, &bytes, false).await
}

// Readers can belong to a different LogStore instance, so the mutex cannot
// protect them from a file that has been created but not fully written.
async fn write_atomic(path: &Path, bytes: &[u8], replace: bool) -> Result<(), RuntimeError> {
    let file_name = path
        .file_name()
        .ok_or_else(|| RuntimeError::InvalidSpec("log path has no file name".into()))?;
    let counter = TEMP_FILE_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let temp_path = path.with_file_name(format!(
        ".{}.tmp-{}-{counter}",
        file_name.to_string_lossy(),
        std::process::id()
    ));
    let result = async {
        use tokio::io::AsyncWriteExt;

        let mut file = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)
            .await
            .map_err(storage_error)?;
        file.write_all(bytes).await.map_err(storage_error)?;
        file.sync_all().await.map_err(storage_error)?;
        drop(file);
        if replace {
            match tokio::fs::remove_file(path).await {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(storage_error(error)),
            }
        }
        tokio::fs::rename(&temp_path, path)
            .await
            .map_err(storage_error)
    }
    .await;
    if result.is_err() {
        let _ = tokio::fs::remove_file(&temp_path).await;
    }
    result
}

async fn read_chunks(directory: &Path) -> Result<Vec<DiskChunk>, RuntimeError> {
    let mut entries = tokio::fs::read_dir(directory)
        .await
        .map_err(storage_error)?;
    let mut chunks: Vec<DiskChunk> = Vec::new();
    while let Some(entry) = entries.next_entry().await.map_err(storage_error)? {
        let is_json = entry
            .path()
            .extension()
            .and_then(|extension| extension.to_str())
            == Some("json");
        if entry.file_type().await.map_err(storage_error)?.is_file() && is_json {
            let bytes = tokio::fs::read(entry.path()).await.map_err(storage_error)?;
            chunks.push(serde_json::from_slice(&bytes).map_err(storage_error)?);
        }
    }
    chunks.sort_by_key(|chunk| chunk.start);
    Ok(chunks)
}

fn chunk_file_name(chunk: &DiskChunk) -> String {
    let channel = match chunk.channel {
        LogChannel::Stdout => "stdout",
        LogChannel::Stderr => "stderr",
        LogChannel::System => "system",
    };
    format!("{:020}-{:020}-{channel}.json", chunk.start, chunk.end)
}

fn split_text(mut value: &str, max_bytes: usize) -> Vec<&str> {
    let mut parts = Vec::new();
    while !value.is_empty() {
        let take = char_boundary_at_or_before(value, max_bytes);
        parts.push(&value[..take]);
        value = &value[take..];
    }
    parts
}

fn char_boundary_at_or_before(value: &str, limit: usize) -> usize {
    let mut take = value.len().min(limit);
    while take > 0 && !value.is_char_boundary(take) {
        take -= 1;
    }
    take
}

fn char_boundary_at_or_after(value: &str, offset: usize) -> usize {
    let mut take = value.len().min(offset);
    while take < value.len() && !value.is_char_boundary(take) {
        take += 1;
    }
    take
}

fn redact(value: &str, secret_values: &[&str]) -> String {
    secret_values
        .iter()
        .filter(|secret| secret.len() >= 8)
        .fold(value.into(), |current, secret| {
            current.replace(secret, "[REDACTED]")
        })
}

fn to_u64(value: i64, field: &str) -> Result<u64, RuntimeError> {
    u64::try_from(value)
        .map_err(|_| RuntimeError::InvalidSpec(format!("stored {field} is negative")))
}

fn to_i64(value: u64) -> Result<i64, RuntimeError> {
    i64::try_from(value)
        .map_err(|_| RuntimeError::InvalidSpec("log cursor exceeds SQLite range".into()))
}

fn storage_error(error: impl Into<anyhow::Error>) -> RuntimeError {
    RuntimeError::unavailable(format!("log storage failure: {}", error.into()))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use janus_infrastructure::id::AsyncTaskId;
    use janus_infrastructure::testing::TestDb;
    use tempfile::TempDir;

    use super::{LogRetention, LogStore};
    use crate::interface::{LogChannel, LogCursor, LogOwnerKind};

    async fn test_store() -> anyhow::Result<(TempDir, LogStore, Arc<TestDb>)> {
        let temp = TempDir::new()?;
        let test_db = TestDb::open().await?;
        let store = LogStore::new(test_db.database().clone(), temp.path());
        Ok((temp, store, test_db))
    }

    #[tokio::test]
    async fn sync_streams_are_unique_per_invocation() -> anyhow::Result<()> {
        let (_temp, store, _db) = test_store().await?;
        let first = store.create(LogOwnerKind::Sync, "runtime-1").await?;
        let second = store.create(LogOwnerKind::Sync, "runtime-1").await?;
        assert_ne!(first.id, second.id);
        Ok(())
    }

    #[tokio::test]
    async fn redacts_closes_and_retains_head_marker_and_tail() -> anyhow::Result<()> {
        let (_temp, store, _db) = test_store().await?;
        let stream = store
            .create(LogOwnerKind::AsyncTask, &AsyncTaskId::new().to_string())
            .await?;
        let retention = LogRetention {
            raw_limit_bytes: 96,
            head_bytes: 24,
        };
        let secret = "secret-value-123";
        store
            .append(
                stream.id,
                LogChannel::Stdout,
                format!("head:{secret}\n").as_bytes(),
                &[secret],
                retention,
            )
            .await?;
        let final_projection = store
            .append(
                stream.id,
                LogChannel::Stderr,
                format!("{}tail", "middle".repeat(30)).as_bytes(),
                &[secret],
                retention,
            )
            .await?;
        assert!(final_projection.truncated);
        assert!(final_projection.total_bytes > retention.raw_limit_bytes);

        let range = store
            .read(stream.id, LogCursor::new(0), 1024 * 1024)
            .await?;
        let text = range
            .chunks
            .iter()
            .map(|chunk| chunk.text.as_str())
            .collect::<String>();
        assert!(text.contains("[REDACTED]"));
        assert!(!text.contains(secret));
        assert!(text.contains("Janus log truncated"));
        assert!(text.contains("tail"));
        assert!(
            range
                .chunks
                .windows(2)
                .all(|pair| { pair[0].end_cursor.value() <= pair[1].start_cursor.value() })
        );

        let closed = store.close(stream.id).await?;
        assert!(closed.closed);
        assert!(
            store
                .append(stream.id, LogChannel::Stdout, b"late", &[], retention,)
                .await
                .is_err()
        );
        Ok(())
    }

    #[tokio::test]
    async fn unicode_retention_and_cursor_ranges_use_utf8_boundaries() -> anyhow::Result<()> {
        let (_temp, store, _db) = test_store().await?;
        let retention = LogRetention {
            raw_limit_bytes: 47,
            head_bytes: 5,
        };
        let retained = store
            .create(LogOwnerKind::AsyncTask, &AsyncTaskId::new().to_string())
            .await?;
        store
            .append(
                retained.id,
                LogChannel::Stdout,
                "头🙂中间🙂尾部".repeat(8).as_bytes(),
                &[],
                retention,
            )
            .await?;
        let retained_range = store.read(retained.id, LogCursor::ZERO, 1024).await?;
        assert!(
            retained_range
                .chunks
                .iter()
                .any(|chunk| chunk.text.contains("Janus log truncated"))
        );

        let ranged = store
            .create(LogOwnerKind::AsyncTask, &AsyncTaskId::new().to_string())
            .await?;
        store
            .append(
                ranged.id,
                LogChannel::Stdout,
                "甲🙂乙".as_bytes(),
                &[],
                LogRetention::ASYNC_TASK,
            )
            .await?;
        let first = store.read(ranged.id, LogCursor::ZERO, 3).await?;
        assert_eq!(first.chunks[0].text, "甲");
        assert_eq!(first.chunks[0].end_cursor.value(), 3);
        let emoji = store.read(ranged.id, LogCursor::new(3), 4).await?;
        assert_eq!(emoji.chunks[0].text, "🙂");
        assert_eq!(emoji.chunks[0].end_cursor.value(), 7);
        let inside = store.read(ranged.id, LogCursor::new(4), 16).await?;
        assert_eq!(inside.chunks[0].start_cursor.value(), 7);
        assert_eq!(inside.chunks[0].text, "乙");
        Ok(())
    }
}
