use std::collections::BTreeMap;

use janus_infrastructure::{
    database::Database, events::EventStore, managed_storage::BlobStore,
    operations::OperationInterface, secrets::SecretCipher,
};
use janus_models::interface::{
    EmbeddedModelInput, ModelClient, ModelsError, ModelsInterface, ProviderInput, ProviderKind,
};
use janus_projects::interface::{
    EgressScheme, ProjectCliConfigInput, ProjectEgressRuleInput, ProjectRuntimeConfigInput,
    ProjectRuntimeSecretInput, ProjectsError, ProjectsInterface,
};
use janus_runtime::interface::*;
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
    projects: ProjectsInterface,
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
            projects,
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

fn limits() -> ResourceLimits {
    ResourceLimits {
        timeout_ms: 30_000,
        memory_bytes: 512 * 1024 * 1024,
        cpu_millis: 1_000,
        pids: 64,
        temporary_disk_bytes: 256 * 1024 * 1024,
        open_files: 256,
    }
}

#[tokio::test]
async fn project_runtime_configuration_is_versioned_validated_and_redacted() -> anyhow::Result<()> {
    let fx = Fx::new().await?;
    let input = ProjectRuntimeConfigInput {
        executor: ExecutorKind::Local,
        allow_insecure_local_executor: true,
        variables: BTreeMap::from([("RUST_LOG".into(), "info".into())]),
        default_limits: limits(),
        network_policy: NetworkPolicy::ProjectRules,
    };
    let first = fx
        .projects
        .save_runtime_config(OWNER_ID, PROJECT_ID, None, input.clone())
        .await?;
    let second = fx
        .projects
        .save_runtime_config(OWNER_ID, PROJECT_ID, Some(&first.version), input)
        .await?;
    assert_ne!(first.version, second.version);
    assert!(matches!(
        fx.projects
            .save_runtime_config(
                OWNER_ID,
                PROJECT_ID,
                Some(&first.version),
                ProjectRuntimeConfigInput {
                    executor: ExecutorKind::Local,
                    allow_insecure_local_executor: true,
                    variables: BTreeMap::new(),
                    default_limits: limits(),
                    network_policy: NetworkPolicy::DenyAll,
                },
            )
            .await,
        Err(ProjectsError::RevisionMismatch { .. })
    ));

    let plaintext = "never-store-this-in-plaintext";
    let secret = fx
        .projects
        .put_runtime_secret(
            OWNER_ID,
            PROJECT_ID,
            ProjectRuntimeSecretInput {
                name: "OPENAI_API_KEY".into(),
                value: plaintext.into(),
            },
        )
        .await?;
    assert!(!serde_json::to_string(&secret)?.contains(plaintext));
    let stored: Vec<u8> =
        sqlx::query_scalar("SELECT value_ciphertext FROM project_runtime_secrets WHERE id = ?")
            .bind(&secret.id)
            .fetch_one(&fx.pool)
            .await?;
    assert!(
        !stored
            .windows(plaintext.len())
            .any(|bytes| bytes == plaintext.as_bytes())
    );
    assert_eq!(
        fx.projects
            .runtime_secret_value(OWNER_ID, PROJECT_ID, &secret.id)
            .await?
            .expose(),
        plaintext
    );

    let rules = fx
        .projects
        .replace_egress_rules(
            OWNER_ID,
            PROJECT_ID,
            vec![ProjectEgressRuleInput {
                scheme: EgressScheme::Https,
                host: " API.Example.COM ".into(),
                port_start: 443,
                port_end: 443,
                purpose: " model API ".into(),
            }],
        )
        .await?;
    assert_eq!(rules[0].host, "api.example.com");
    assert_eq!(rules[0].purpose, "model API");
    assert!(matches!(
        fx.projects
            .replace_egress_rules(
                OWNER_ID,
                PROJECT_ID,
                vec![ProjectEgressRuleInput {
                    scheme: EgressScheme::Https,
                    host: "*.example.com".into(),
                    port_start: 443,
                    port_end: 443,
                    purpose: "wildcard".into(),
                }],
            )
            .await,
        Err(ProjectsError::Validation(_))
    ));

    let cli = fx
        .projects
        .save_cli_config(
            OWNER_ID,
            PROJECT_ID,
            ProjectCliConfigInput {
                kind: DelegatedCliKind::Codex,
                enabled: true,
                secret_id: Some(secret.id),
                options: BTreeMap::from([
                    ("approval_policy".into(), "never".into()),
                    ("sandbox_mode".into(), "workspace-write".into()),
                ]),
            },
        )
        .await?;
    assert!(cli.enabled);
    assert!(matches!(
        fx.projects
            .save_cli_config(
                OWNER_ID,
                PROJECT_ID,
                ProjectCliConfigInput {
                    kind: DelegatedCliKind::Codex,
                    enabled: true,
                    secret_id: None,
                    options: BTreeMap::from([("dangerous_flag".into(), "yes".into())]),
                },
            )
            .await,
        Err(ProjectsError::Validation(_))
    ));
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

#[test]
fn capability_evaluator_is_exhaustive_and_enforces_production_local_policy() {
    let probe = DeploymentCapabilityProbe {
        platform_supports_container: false,
        podman_available: false,
        podman_probe_failed: false,
        browser_available: false,
        claude_code_available: true,
        codex_available: false,
        checked_at: NOW.into(),
    };
    let capabilities = RuntimeCapabilityEvaluator::effective(
        &probe,
        EffectiveCapabilityConfig {
            executor: ExecutorKind::Local,
            production: true,
            allow_insecure_local_executor: false,
            bash_egress_configured: true,
            live_preview_configured: false,
            scope: CapabilityScope::Session,
        },
    );
    assert_eq!(capabilities.len(), RuntimeCapabilityId::ALL.len());
    assert!(capabilities.iter().all(|value| {
        value.scope == CapabilityScope::Session && value.checked_at.as_deref() == Some(NOW)
    }));
    let process = capabilities
        .iter()
        .find(|value| value.id == RuntimeCapabilityId::ProcessExecution)
        .expect("process capability");
    assert_eq!(process.state, CapabilityState::Unconfigured);
    assert_eq!(process.reason_code, Some(CapabilityReason::PolicyDisabled));
    let claude = capabilities
        .iter()
        .find(|value| value.id == RuntimeCapabilityId::DelegatedCliClaudeCode)
        .expect("Claude capability");
    assert_eq!(claude.state, CapabilityState::Ready);
    assert_eq!(claude.reason_code, None);
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
            "INSERT INTO runtimes (id, scope_kind, scope_id, executor_kind, executor_nonce, \
             limits_json, capability_snapshot_json, status, version, created_at, updated_at) \
             VALUES (?, ?, ?, 'local', 'nonce', '{}', '{}', 'ready', 'v1', ?, ?)",
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
    insert_runtime(&fx.pool, "runtime-session", "session", "session-shared").await?;
    let duplicate = insert_runtime(&fx.pool, "runtime-session-dup", "session", "session-shared").await;
    assert!(duplicate.is_err(), "duplicate session-scoped Runtime was accepted");
    // A different scope_kind is a different unique-index key and must be allowed.
    insert_runtime(&fx.pool, "runtime-project", "project", "session-shared").await?;
    Ok(())
}

