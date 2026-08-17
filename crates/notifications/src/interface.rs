//! Public notification-channel capability boundary.

use std::time::Duration;

use janus_infrastructure::{
    clock::now_utc_str,
    events::{EventStore, EventType, NewEvent},
    id::NotificationChannelId,
    secrets::{Secret, SecretCipher},
    unit_of_work::{UnitOfWork, UnitOfWorkTransaction},
};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{FromRow, SqlitePool};
use thiserror::Error;
use url::Url;
use utoipa::ToSchema;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum NotificationChannelKind {
    Webhook,
    Qqbot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum NotificationEventKind {
    TurnCompleted,
    TurnFailed,
    AsyncTaskCompleted,
    Test,
}

impl NotificationEventKind {
    pub const CONFIGURABLE: [Self; 3] = [
        Self::TurnCompleted,
        Self::TurnFailed,
        Self::AsyncTaskCompleted,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::TurnCompleted => "turn_completed",
            Self::TurnFailed => "turn_failed",
            Self::AsyncTaskCompleted => "async_task_completed",
            Self::Test => "test",
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct NotificationTarget {
    pub user_id: Option<String>,
    pub group_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct NotificationChannelInput {
    pub kind: NotificationChannelKind,
    pub display_name: String,
    pub endpoint_url: String,
    #[schema(write_only)]
    pub secret: Option<String>,
    #[serde(default)]
    pub target: NotificationTarget,
    #[serde(default = "default_events")]
    pub events: Vec<NotificationEventKind>,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct NotificationChannelView {
    pub id: String,
    pub kind: NotificationChannelKind,
    pub display_name: String,
    pub endpoint_url: String,
    pub secret_is_set: bool,
    pub target: NotificationTarget,
    pub events: Vec<NotificationEventKind>,
    pub enabled: bool,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct NotificationEvent {
    pub kind: NotificationEventKind,
    pub title: String,
    pub message: String,
    pub data: Value,
}

#[derive(Debug, Error)]
pub enum NotificationsError {
    #[error("the notification configuration is invalid: {0}")]
    Validation(String),
    #[error("the notification channel was not found")]
    ChannelNotFound,
    #[error("notification storage failed")]
    Storage(#[from] sqlx::Error),
    #[error("notification data is invalid")]
    Data(#[from] serde_json::Error),
    #[error("notification operation failed")]
    Internal(#[from] anyhow::Error),
    #[error("notification delivery failed: {0}")]
    Delivery(String),
}

#[derive(Clone)]
pub struct NotificationsInterface {
    pool: SqlitePool,
    unit_of_work: UnitOfWork,
    cipher: SecretCipher,
    client: Client,
}

#[derive(Debug, FromRow)]
struct ChannelRow {
    id: String,
    owner_id: String,
    kind: String,
    display_name: String,
    endpoint_url: String,
    secret_ciphertext: Option<Vec<u8>>,
    target_json: String,
    events_json: String,
    enabled: i64,
    created_at: String,
    updated_at: String,
}

impl NotificationsInterface {
    pub fn new(pool: SqlitePool, cipher: SecretCipher, events: EventStore) -> anyhow::Result<Self> {
        Ok(Self {
            unit_of_work: UnitOfWork::new(pool.clone(), events),
            pool,
            cipher,
            client: Client::builder()
                .connect_timeout(Duration::from_secs(8))
                .timeout(Duration::from_secs(15))
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
        })
    }

    pub async fn channels(
        &self,
        owner_id: &str,
    ) -> Result<Vec<NotificationChannelView>, NotificationsError> {
        let rows = sqlx::query_as::<_, ChannelRow>(
            "SELECT id, owner_id, kind, display_name, endpoint_url, secret_ciphertext, \
             target_json, events_json, enabled, created_at, updated_at \
             FROM notification_channels WHERE owner_id = ? ORDER BY display_name, id",
        )
        .bind(owner_id)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(channel_view).collect()
    }

    pub async fn create_channel(
        &self,
        owner_id: &str,
        input: NotificationChannelInput,
        correlation_id: &str,
    ) -> Result<NotificationChannelView, NotificationsError> {
        validate_input(&input, true)?;
        let id = NotificationChannelId::new().to_string();
        let now = now_utc_str();
        let secret = encrypt_secret(&self.cipher, owner_id, &id, input.secret.as_deref())?;
        let target_json = serde_json::to_string(&input.target)?;
        let events_json = serde_json::to_string(&input.events)?;
        let mut work = self.unit_of_work.begin().await?;
        sqlx::query(
            "INSERT INTO notification_channels \
             (id, owner_id, kind, display_name, endpoint_url, secret_ciphertext, target_json, \
              events_json, enabled, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&id)
        .bind(owner_id)
        .bind(kind_str(input.kind))
        .bind(input.display_name.trim())
        .bind(normalize_url(&input.endpoint_url)?)
        .bind(secret)
        .bind(target_json)
        .bind(events_json)
        .bind(input.enabled)
        .bind(&now)
        .bind(&now)
        .execute(work.connection())
        .await?;
        append_changed(&mut work, owner_id, &id, "created", correlation_id).await?;
        work.commit().await?;
        self.channel(owner_id, &id).await
    }

    pub async fn update_channel(
        &self,
        owner_id: &str,
        id: &str,
        input: NotificationChannelInput,
        correlation_id: &str,
    ) -> Result<NotificationChannelView, NotificationsError> {
        validate_input(&input, false)?;
        let existing = self.channel_row(owner_id, id).await?;
        let secret = match input.secret.as_deref() {
            Some(value) => encrypt_secret(&self.cipher, owner_id, id, Some(value))?,
            None => existing.secret_ciphertext,
        };
        let now = now_utc_str();
        let target_json = serde_json::to_string(&input.target)?;
        let events_json = serde_json::to_string(&input.events)?;
        let mut work = self.unit_of_work.begin().await?;
        let changed = sqlx::query(
            "UPDATE notification_channels SET kind=?, display_name=?, endpoint_url=?, \
             secret_ciphertext=?, target_json=?, events_json=?, enabled=?, updated_at=? \
             WHERE id=? AND owner_id=?",
        )
        .bind(kind_str(input.kind))
        .bind(input.display_name.trim())
        .bind(normalize_url(&input.endpoint_url)?)
        .bind(secret)
        .bind(target_json)
        .bind(events_json)
        .bind(input.enabled)
        .bind(&now)
        .bind(id)
        .bind(owner_id)
        .execute(work.connection())
        .await?
        .rows_affected();
        if changed == 0 {
            work.rollback().await?;
            return Err(NotificationsError::ChannelNotFound);
        }
        append_changed(&mut work, owner_id, id, "updated", correlation_id).await?;
        work.commit().await?;
        self.channel(owner_id, id).await
    }

    pub async fn delete_channel(
        &self,
        owner_id: &str,
        id: &str,
        correlation_id: &str,
    ) -> Result<(), NotificationsError> {
        let mut work = self.unit_of_work.begin().await?;
        let changed =
            sqlx::query("DELETE FROM notification_channels WHERE id = ? AND owner_id = ?")
                .bind(id)
                .bind(owner_id)
                .execute(work.connection())
                .await?
                .rows_affected();
        if changed == 0 {
            work.rollback().await?;
            return Err(NotificationsError::ChannelNotFound);
        }
        append_changed(&mut work, owner_id, id, "deleted", correlation_id).await?;
        work.commit().await?;
        Ok(())
    }

    pub async fn test_channel(&self, owner_id: &str, id: &str) -> Result<(), NotificationsError> {
        let row = self.channel_row(owner_id, id).await?;
        self.send_row(
            &row,
            &NotificationEvent {
                kind: NotificationEventKind::Test,
                title: "Janus notification test".into(),
                message: "The configured notification channel is reachable.".into(),
                data: json!({"channel_id": id}),
            },
        )
        .await
    }

    /// Deliver a committed application event to all matching enabled channels.
    /// This method owns only channel lookup and outbound side effects; event
    /// selection and owner resolution remain in server application workflows.
    pub async fn dispatch(
        &self,
        owner_id: &str,
        event: &NotificationEvent,
    ) -> Result<(), NotificationsError> {
        let rows = sqlx::query_as::<_, ChannelRow>(
            "SELECT id, owner_id, kind, display_name, endpoint_url, secret_ciphertext, \
             target_json, events_json, enabled, created_at, updated_at \
             FROM notification_channels WHERE owner_id = ? AND enabled = 1 \
             ORDER BY id",
        )
        .bind(owner_id)
        .fetch_all(&self.pool)
        .await?;
        let mut errors = Vec::new();
        for row in rows {
            let events: Vec<NotificationEventKind> = serde_json::from_str(&row.events_json)?;
            if event.kind != NotificationEventKind::Test && !events.contains(&event.kind) {
                continue;
            }
            if let Err(error) = self.send_row(&row, event).await {
                errors.push(format!("{}: {error}", row.display_name));
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(NotificationsError::Delivery(errors.join("; ")))
        }
    }

    async fn send_row(
        &self,
        row: &ChannelRow,
        event: &NotificationEvent,
    ) -> Result<(), NotificationsError> {
        let target: NotificationTarget = serde_json::from_str(&row.target_json)?;
        let secret = row
            .secret_ciphertext
            .as_deref()
            .map(|stored| {
                self.cipher
                    .decrypt(stored, &secret_aad(&row.owner_id, &row.id))
            })
            .transpose()?;
        let response = match parse_kind(&row.kind)? {
            NotificationChannelKind::Webhook => {
                let mut request = self.client.post(&row.endpoint_url).json(&json!({
                    "event": event.kind.as_str(),
                    "title": event.title,
                    "message": event.message,
                    "data": event.data,
                }));
                if let Some(secret) = secret {
                    request = request.bearer_auth(secret.expose());
                }
                request.send().await
            }
            NotificationChannelKind::Qqbot => {
                let mut body = serde_json::Map::new();
                if let Some(group_id) = target.group_id {
                    body.insert("message_type".into(), json!("group"));
                    body.insert("group_id".into(), json!(group_id));
                } else if let Some(user_id) = target.user_id {
                    body.insert("message_type".into(), json!("private"));
                    body.insert("user_id".into(), json!(user_id));
                } else {
                    return Err(NotificationsError::Validation(
                        "qqbot channel target is missing".into(),
                    ));
                }
                body.insert(
                    "message".into(),
                    json!(format!("{}\n\n{}", event.title, event.message)),
                );
                let mut request = self.client.post(&row.endpoint_url).json(&body);
                if let Some(secret) = secret {
                    request = request.bearer_auth(secret.expose());
                }
                request.send().await
            }
        }
        .map_err(|error| NotificationsError::Delivery(error.to_string()))?;
        if response.status().is_success() {
            Ok(())
        } else {
            Err(NotificationsError::Delivery(format!(
                "endpoint returned HTTP {}",
                response.status()
            )))
        }
    }

    async fn channel(
        &self,
        owner_id: &str,
        id: &str,
    ) -> Result<NotificationChannelView, NotificationsError> {
        channel_view(self.channel_row(owner_id, id).await?)
    }

    async fn channel_row(
        &self,
        owner_id: &str,
        id: &str,
    ) -> Result<ChannelRow, NotificationsError> {
        sqlx::query_as::<_, ChannelRow>(
            "SELECT id, owner_id, kind, display_name, endpoint_url, secret_ciphertext, \
             target_json, events_json, enabled, created_at, updated_at \
             FROM notification_channels WHERE id = ? AND owner_id = ?",
        )
        .bind(id)
        .bind(owner_id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or(NotificationsError::ChannelNotFound)
    }
}

fn default_true() -> bool {
    true
}

fn default_events() -> Vec<NotificationEventKind> {
    NotificationEventKind::CONFIGURABLE.to_vec()
}

fn validate_input(
    input: &NotificationChannelInput,
    creating: bool,
) -> Result<(), NotificationsError> {
    if input.display_name.trim().is_empty() {
        return Err(NotificationsError::Validation(
            "display_name is required".into(),
        ));
    }
    normalize_url(&input.endpoint_url)?;
    if input.events.is_empty() || input.events.contains(&NotificationEventKind::Test) {
        return Err(NotificationsError::Validation(
            "events must contain at least one configurable event".into(),
        ));
    }
    if input
        .events
        .iter()
        .any(|event| !NotificationEventKind::CONFIGURABLE.contains(event))
    {
        return Err(NotificationsError::Validation(
            "unknown notification event".into(),
        ));
    }
    match input.kind {
        NotificationChannelKind::Webhook => {}
        NotificationChannelKind::Qqbot => {
            let has_user = input
                .target
                .user_id
                .as_deref()
                .is_some_and(|value| !value.trim().is_empty());
            let has_group = input
                .target
                .group_id
                .as_deref()
                .is_some_and(|value| !value.trim().is_empty());
            if has_user == has_group {
                return Err(NotificationsError::Validation(
                    "qqbot target must contain exactly one user_id or group_id".into(),
                ));
            }
            if creating
                && input
                    .secret
                    .as_deref()
                    .is_none_or(|value| value.trim().is_empty())
            {
                return Err(NotificationsError::Validation(
                    "qqbot secret is required".into(),
                ));
            }
        }
    }
    if input
        .secret
        .as_deref()
        .is_some_and(|value| value.trim().is_empty())
    {
        return Err(NotificationsError::Validation(
            "secret cannot be empty".into(),
        ));
    }
    Ok(())
}

fn normalize_url(value: &str) -> Result<String, NotificationsError> {
    let mut url = Url::parse(value.trim()).map_err(|_| {
        NotificationsError::Validation("endpoint_url must be an absolute URL".into())
    })?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err(NotificationsError::Validation(
            "endpoint_url must use http or https".into(),
        ));
    }
    url.set_fragment(None);
    Ok(url.to_string())
}

fn kind_str(kind: NotificationChannelKind) -> &'static str {
    match kind {
        NotificationChannelKind::Webhook => "webhook",
        NotificationChannelKind::Qqbot => "qqbot",
    }
}

fn parse_kind(value: &str) -> Result<NotificationChannelKind, NotificationsError> {
    match value {
        "webhook" => Ok(NotificationChannelKind::Webhook),
        "qqbot" => Ok(NotificationChannelKind::Qqbot),
        _ => Err(NotificationsError::Validation(
            "unknown notification channel kind".into(),
        )),
    }
}

fn encrypt_secret(
    cipher: &SecretCipher,
    owner_id: &str,
    id: &str,
    value: Option<&str>,
) -> Result<Option<Vec<u8>>, NotificationsError> {
    value
        .filter(|value| !value.trim().is_empty())
        .map(|value| {
            cipher.encrypt(
                &Secret::new(value.trim().to_owned()),
                &secret_aad(owner_id, id),
            )
        })
        .transpose()
        .map_err(Into::into)
}

fn secret_aad(owner_id: &str, id: &str) -> String {
    format!("v1/{owner_id}/notification_channels/{id}/secret")
}

fn channel_view(row: ChannelRow) -> Result<NotificationChannelView, NotificationsError> {
    Ok(NotificationChannelView {
        id: row.id,
        kind: parse_kind(&row.kind)?,
        display_name: row.display_name,
        endpoint_url: row.endpoint_url,
        secret_is_set: row.secret_ciphertext.is_some(),
        target: serde_json::from_str(&row.target_json)?,
        events: serde_json::from_str(&row.events_json)?,
        enabled: row.enabled != 0,
        created_at: row.created_at,
        updated_at: row.updated_at,
    })
}

async fn append_changed(
    work: &mut UnitOfWorkTransaction<'_>,
    owner_id: &str,
    channel_id: &str,
    operation: &str,
    correlation_id: &str,
) -> Result<(), NotificationsError> {
    work.append_event(NewEvent {
        event_type: EventType::NotificationChannelChanged,
        actor: json!({"kind": "owner", "id": owner_id}),
        resource: Some(json!({"kind": "notification_channel", "id": channel_id})),
        correlation_id: correlation_id.to_owned(),
        causation_id: None,
        payload: json!({"operation": operation}),
    })
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(
        kind: NotificationChannelKind,
        secret: Option<&str>,
        target: NotificationTarget,
    ) -> NotificationChannelInput {
        NotificationChannelInput {
            kind,
            display_name: "test channel".into(),
            endpoint_url: "https://example.test/janus#ignored".into(),
            secret: secret.map(str::to_owned),
            target,
            events: vec![NotificationEventKind::TurnCompleted],
            enabled: true,
        }
    }

    #[test]
    fn webhook_can_omit_secret() {
        assert!(
            validate_input(
                &input(
                    NotificationChannelKind::Webhook,
                    None,
                    NotificationTarget::default()
                ),
                true,
            )
            .is_ok()
        );
        assert_eq!(
            normalize_url("https://example.test/janus#ignored").expect("valid URL"),
            "https://example.test/janus"
        );
    }

    #[test]
    fn qqbot_requires_one_target_and_secret_when_created() {
        let valid = input(
            NotificationChannelKind::Qqbot,
            Some("bot-token"),
            NotificationTarget {
                user_id: None,
                group_id: Some("123".into()),
            },
        );
        assert!(validate_input(&valid, true).is_ok());

        let missing_secret = input(
            NotificationChannelKind::Qqbot,
            None,
            NotificationTarget {
                user_id: None,
                group_id: Some("123".into()),
            },
        );
        assert!(matches!(
            validate_input(&missing_secret, true),
            Err(NotificationsError::Validation(detail)) if detail.contains("secret is required")
        ));

        let two_targets = input(
            NotificationChannelKind::Qqbot,
            Some("bot-token"),
            NotificationTarget {
                user_id: Some("456".into()),
                group_id: Some("123".into()),
            },
        );
        assert!(matches!(
            validate_input(&two_targets, true),
            Err(NotificationsError::Validation(detail)) if detail.contains("exactly one")
        ));
    }

    #[test]
    fn qqbot_update_can_keep_existing_secret() {
        let update = input(
            NotificationChannelKind::Qqbot,
            None,
            NotificationTarget {
                user_id: Some("456".into()),
                group_id: None,
            },
        );
        assert!(validate_input(&update, false).is_ok());
    }

    #[test]
    fn notification_events_cannot_include_test_event_in_configuration() {
        let mut channel = input(
            NotificationChannelKind::Webhook,
            None,
            NotificationTarget::default(),
        );
        channel.events = vec![NotificationEventKind::Test];
        assert!(matches!(
            validate_input(&channel, true),
            Err(NotificationsError::Validation(detail)) if detail.contains("configurable event")
        ));
    }
}
