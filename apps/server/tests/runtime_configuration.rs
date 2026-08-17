use std::collections::BTreeMap;

use janus_infrastructure::{
    database::Database, events::EventStore, managed_storage::BlobStore,
    operations::OperationInterface, secrets::SecretCipher,
};
use janus_models::interface::{
    EmbeddedModelInput, ModelClient, ModelsError, ModelsInterface, ProviderInput, ProviderKind,
};
use janus_projects::interface::ProjectsInterface;
use janus_workspace::interface::WorkspaceInterface;
use sqlx::SqlitePool;
use tempfile::TempDir;

const OWNER_ID: &str = "owner-test";
const PROJECT_ID: &str = "project-test";
const NOW: &str = "2026-01-01T00:00:00.000Z";

struct Fx {
    _temp: TempDir,
    _database: Database,
    pool: SqlitePool,
    _projects: ProjectsInterface,
    models: ModelsInterface,
}

impl Fx {
    async fn new() -> anyhow::Result<Self> {
        let temp = TempDir::new()?;
        let database = Database::open(temp.path(), janus_server::migrator()).await?;
        let pool = database.pool().clone();
        seed_owner_and_project(&pool).await?;
        let cipher = SecretCipher::load(temp.path(), false)?;
        let blobs = BlobStore::new(pool.clone(), temp.path())?;
        let workspace = WorkspaceInterface::new(pool.clone(), temp.path(), blobs);
        let events = EventStore::new(pool.clone());
        let projects = ProjectsInterface::new(
            pool.clone(),
            cipher.clone(),
            OperationInterface::new(pool.clone(), events.clone()),
            workspace,
            events,
            temp.path(),
        );
        let models = ModelsInterface::new(pool.clone(), cipher, EventStore::new(pool.clone()))?;
        Ok(Self {
            _temp: temp,
            _database: database,
            pool,
            _projects: projects,
            models,
        })
    }
}

async fn seed_owner_and_project(pool: &SqlitePool) -> anyhow::Result<()> {
    sqlx::query(
        "INSERT INTO owners (id, display_name, created_at) \
         VALUES (?, 'Owner', ?)",
    )
    .bind(OWNER_ID)
    .bind(NOW)
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO projects \
         (id, owner_id, name, state, repo_access, repo_url, version, \
          created_at, updated_at, last_activity_at) \
         VALUES (?, ?, 'Project', 'ready', 'public_https', \
                 'https://example.com/repo.git', 'v1', ?, ?, ?)",
    )
    .bind(PROJECT_ID)
    .bind(OWNER_ID)
    .bind(NOW)
    .bind(NOW)
    .bind(NOW)
    .execute(pool)
    .await?;
    Ok(())
}
fn provider_input(models: Vec<EmbeddedModelInput>) -> ProviderInput {
    ProviderInput {
        client: ModelClient::Supervisor,
        kind: ProviderKind::OpenaiResponses,
        display_name: "Provider".into(),
        base_url: "https://api.example.com/v1".into(),
        api_key: None,
        models,
        enabled: true,
    }
}

fn model(name: &str, upstream: &str, supports_1m: bool) -> EmbeddedModelInput {
    EmbeddedModelInput {
        display_name: name.into(),
        upstream_model_id: upstream.into(),
        supports_1m,
        supports_images: false,
        enabled: true,
    }
}

#[tokio::test]
async fn normalized_models_keep_ids_and_validate_ordered_failover() -> anyhow::Result<()> {
    let fx = Fx::new().await?;
    let provider = fx
        .models
        .create_provider(
            OWNER_ID,
            provider_input(vec![
                model("Primary", "model-a", false),
                model("Fallback B", "model-b", true),
                model("Fallback C", "model-c", false),
            ]),
            "test-model-config",
        )
        .await?;
    let before = fx.models.models(OWNER_ID).await?;
    assert_eq!(before.len(), 3);
    assert_eq!(before[0].context_limit, 1_000_000);
    let ids: BTreeMap<_, _> = before
        .iter()
        .map(|value| (value.upstream_model_id.clone(), value.id.clone()))
        .collect();

    fx.models
        .update_provider(
            OWNER_ID,
            &provider.id,
            provider_input(vec![
                model("Primary renamed", "model-a", false),
                model("Fallback B", "model-b", true),
                model("Fallback C", "model-c", false),
            ]),
            "test-model-config",
        )
        .await?;
    let after = fx.models.models(OWNER_ID).await?;
    for value in &after {
        assert_eq!(ids[&value.upstream_model_id], value.id);
    }

    let primary = &ids["model-a"];
    let candidates = vec![ids["model-b"].clone(), ids["model-c"].clone()];
    fx.models
        .set_failover(OWNER_ID, primary, candidates.clone(), "test-model-config")
        .await?;
    assert_eq!(
        fx.models.failover(OWNER_ID, primary).await?.candidates,
        candidates
    );
    assert!(matches!(
        fx.models
            .set_failover(
                OWNER_ID,
                primary,
                vec![primary.clone()],
                "test-model-config",
            )
            .await,
        Err(ModelsError::Validation(_))
    ));
    Ok(())
}

#[tokio::test]
async fn runtime_scope_uniqueness_rejects_duplicate_scope() -> anyhow::Result<()> {
    let fx = Fx::new().await?;
    async fn insert_runtime(
        pool: &SqlitePool,
        id: &str,
        scope_kind: &str,
        scope_id: &str,
    ) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO runtimes (id, scope_kind, scope_id, executor_nonce, limits_json, \
             status, version, created_at, updated_at) \
             VALUES (?, ?, ?, 'nonce', '{}', 'ready', 'v1', ?, ?)",
        )
        .bind(id)
        .bind(scope_kind)
        .bind(scope_id)
        .bind(NOW)
        .bind(NOW)
        .execute(pool)
        .await?;
        Ok(())
    }
    insert_runtime(&fx.pool, "runtime-project", "project", "project-shared").await?;
    let duplicate =
        insert_runtime(&fx.pool, "runtime-project-dup", "project", "project-shared").await;
    assert!(
        duplicate.is_err(),
        "duplicate project-scoped Runtime was accepted"
    );
    insert_runtime(
        &fx.pool,
        "runtime-project-other",
        "project",
        "project-other",
    )
    .await?;
    Ok(())
}
