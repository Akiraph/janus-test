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
use mongodb::bson::{doc, Document};
use tempfile::TempDir;

const OWNER_ID: &str = "owner-test";
const PROJECT_ID: &str = "project-test";
const NOW: &str = "2026-01-01T00:00:00.000Z";

static TEST_DB_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

struct Fx {
    _temp: TempDir,
    _database: Database,
    pool: mongodb::Database,
    _projects: ProjectsInterface,
    models: ModelsInterface,
}

impl Fx {
    async fn new() -> anyhow::Result<Self> {
        let temp = TempDir::new()?;
        let database = Database::open(
            temp.path(),
            &std::env::var("JANUS_MONGODB_URI")
                .unwrap_or_else(|_| "mongodb://localhost:27017/?replicaSet=rs0".into()),
            &format!(
                "janus_test_{}_{}",
                std::process::id(),
                TEST_DB_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ),
        )
        .await?;
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

async fn seed_owner_and_project(pool: &mongodb::Database) -> anyhow::Result<()> {
    pool.collection::<Document>("owners")
        .insert_one(doc! {
            "_id": OWNER_ID,
            "display_name": "Owner",
            "created_at": NOW,
        })
        .await?;
    pool.collection::<Document>("projects")
        .insert_one(doc! {
            "_id": PROJECT_ID,
            "owner_id": OWNER_ID,
            "name": "Project",
            "state": "ready",
            "repo_access": "public_https",
            "repo_url": "https://example.com/repo.git",
            "repo_branch": null,
            "github_credential_id": null,
            "default_model_id": null,
            "main_workspace_handle": null,
            "clone_error": null,
            "version": "v1",
            "created_at": NOW,
            "updated_at": NOW,
            "last_activity_at": NOW,
        })
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
        pool: &mongodb::Database,
        id: &str,
        scope_kind: &str,
        scope_id: &str,
    ) -> anyhow::Result<()> {
        pool.collection::<Document>("runtimes")
            .insert_one(doc! {
                "_id": id,
                "scope_kind": scope_kind,
                "scope_id": scope_id,
                "executor_nonce": "nonce",
                "limits_json": "{}",
                "status": "ready",
                "version": "v1",
                "created_at": NOW,
                "updated_at": NOW,
            })
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
