use std::{borrow::Cow, collections::BTreeMap, str::FromStr};

use anyhow::Context;
use janus_server::{
    config::RunMode,
    modules::{
        models::interface::{
            EmbeddedModelInput, ModelsError, ModelsInterface, ProviderInput, ProviderKind,
        },
        projects::interface::{
            EgressScheme, ProjectCliConfigInput, ProjectEgressRuleInput, ProjectRuntimeConfigInput,
            ProjectRuntimeSecretInput, ProjectsError, ProjectsInterface,
        },
        runtime::interface::{
            CapabilityReason, CapabilityScope, CapabilityState, DelegatedCliKind,
            DeploymentCapabilityProbe, EffectiveCapabilityConfig, ExecutorKind, NetworkPolicy,
            ResourceLimits, RuntimeCapabilityEvaluator, RuntimeCapabilityId,
        },
        workspace_sync::interface::WorkspaceSyncInterface,
    },
    platform::{
        database::Database, managed_storage::BlobStore, operations::OperationInterface,
        secret::SecretCipher,
    },
};
use sqlx::{
    SqlitePool,
    sqlite::{SqliteConnectOptions, SqlitePoolOptions},
};
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
        let database = Database::open(temp.path()).await?;
        let pool = database.pool().clone();
        seed_owner_and_project(&pool).await?;
        let cipher = SecretCipher::load(temp.path(), RunMode::Development)?;
        let blobs = BlobStore::new(pool.clone(), temp.path())?;
        let workspace = WorkspaceSyncInterface::new(pool.clone(), temp.path(), blobs);
        let projects = ProjectsInterface::new(
            pool.clone(),
            cipher.clone(),
            OperationInterface::new(pool.clone()),
            workspace,
            temp.path(),
        );
        let models = ModelsInterface::new(pool.clone(), cipher)?;
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
    sqlx::query("INSERT INTO tenants (id, created_at) VALUES ('tenant-test', ?)")
        .bind(NOW)
        .execute(pool)
        .await?;
    sqlx::query(
        "INSERT INTO owners (id, tenant_id, display_name, created_at) \
         VALUES (?, 'tenant-test', 'Owner', ?)",
    )
    .bind(OWNER_ID)
    .bind(NOW)
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO projects \
         (id, owner_id, tenant_id, name, state, repo_access, repo_url, version, \
          created_at, updated_at, last_activity_at) \
         VALUES (?, ?, 'tenant-test', 'Project', 'ready', 'public_https', \
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
        )
        .await?;
    let after = fx.models.models(OWNER_ID).await?;
    for value in &after {
        assert_eq!(ids[&value.upstream_model_id], value.id);
    }

    let primary = &ids["model-a"];
    let candidates = vec![ids["model-b"].clone(), ids["model-c"].clone()];
    fx.models
        .set_failover(OWNER_ID, primary, candidates.clone())
        .await?;
    assert_eq!(
        fx.models.failover(OWNER_ID, primary).await?.candidates,
        candidates
    );
    assert!(matches!(
        fx.models
            .set_failover(OWNER_ID, primary, vec![primary.clone()])
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
async fn populated_previous_schema_migrates_active_work_without_replay() -> anyhow::Result<()> {
    let temp = TempDir::new()?;
    let database_path = temp.path().join("old.db");
    let options = SqliteConnectOptions::from_str(&database_path.to_string_lossy())?
        .create_if_missing(true)
        .foreign_keys(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await?;
    let migrator = sqlx::migrate!("./migrations");
    let previous = sqlx::migrate::Migrator {
        migrations: Cow::Owned(
            migrator
                .iter()
                .filter(|migration| migration.version <= 9)
                .cloned()
                .collect(),
        ),
        ignore_missing: false,
        locking: true,
        no_tx: false,
    };
    previous.run(&pool).await?;
    seed_owner_and_project(&pool).await?;
    sqlx::query(
        "INSERT INTO model_providers \
         (id, owner_id, kind, display_name, base_url, models_json, enabled, created_at, updated_at) \
         VALUES ('provider-old', ?, 'openai_responses', 'Old', 'https://api.example.com/v1', \
         ?, 1, ?, ?)",
    )
    .bind(OWNER_ID)
    .bind(
        serde_json::json!([
            {"display_name":"Default", "upstream_model_id":"default", "supports_1m":false},
            {"display_name":"Large", "upstream_model_id":"large", "supports_1m":true}
        ])
        .to_string(),
    )
    .bind(NOW)
    .bind(NOW)
    .execute(&pool)
    .await?;
    sqlx::query(
        "INSERT INTO sessions \
         (id, project_id, kind, state, workspace_handle, active_turn_id, source_main_revision_id, \
          version, created_at, updated_at, last_activity_at) \
         VALUES ('session-old', ?, 'regular', 'active', 'workspace', 'turn-old', 'revision-old', \
                 'v1', ?, ?, ?)",
    )
    .bind(PROJECT_ID)
    .bind(NOW)
    .bind(NOW)
    .bind(NOW)
    .execute(&pool)
    .await?;
    sqlx::query(
        "INSERT INTO turns \
         (id, session_id, sequence, status, model_snapshot_json, version, created_at, updated_at) \
         VALUES ('turn-old', 'session-old', 1, 'running', '{}', 'v1', ?, ?)",
    )
    .bind(NOW)
    .bind(NOW)
    .execute(&pool)
    .await?;
    sqlx::query(
        "INSERT INTO rounds \
         (id, turn_id, sequence, status, version, created_at, updated_at) \
         VALUES ('round-old', 'turn-old', 1, 'running', 'v1', ?, ?)",
    )
    .bind(NOW)
    .bind(NOW)
    .execute(&pool)
    .await?;
    sqlx::query(
        "INSERT INTO tool_calls \
         (id, round_id, ord, tool_name, input_json, status, actor_json, version) \
         VALUES ('tool-old', 'round-old', 0, 'finish', '{}', 'running', '{}', 'v1')",
    )
    .execute(&pool)
    .await?;
    sqlx::query(
        "INSERT INTO model_attempts \
         (id, round_id, provider_id, upstream_model_id, status, created_at) \
         VALUES ('attempt-old', 'round-old', 'provider-old', 'default', 'running', ?)",
    )
    .bind(NOW)
    .execute(&pool)
    .await?;

    migrator.run(&pool).await?;
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT status FROM turns WHERE id = 'turn-old'")
            .fetch_one(&pool)
            .await
            .context("migrated Turn is missing")?,
        "interrupted"
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT status FROM rounds WHERE id = 'round-old'")
            .fetch_one(&pool)
            .await
            .context("migrated Round is missing")?,
        "interrupted"
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT status FROM tool_calls WHERE id = 'tool-old'")
            .fetch_one(&pool)
            .await
            .context("migrated Tool Call is missing")?,
        "lost"
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>(
            "SELECT status FROM model_attempts WHERE id = 'attempt-old'"
        )
        .fetch_one(&pool)
        .await
        .context("migrated model attempt is missing")?,
        "interrupted"
    );
    let session: (String, Option<String>) =
        sqlx::query_as("SELECT state, active_turn_id FROM sessions WHERE id = 'session-old'")
            .fetch_one(&pool)
            .await
            .context("migrated Session is missing")?;
    assert_eq!(session, ("ready".into(), None));
    let limits = sqlx::query_as::<_, (String, i64)>(
        "SELECT upstream_model_id, context_limit FROM models ORDER BY upstream_model_id",
    )
    .fetch_all(&pool)
    .await?;
    assert_eq!(
        limits,
        vec![("default".into(), 200_000), ("large".into(), 1_000_000)]
    );
    Ok(())
}
