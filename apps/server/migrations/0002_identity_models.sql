-- janus-module: identity
-- janus-module: models

CREATE TABLE tenants (
    id TEXT PRIMARY KEY,
    created_at TEXT NOT NULL
);

CREATE TABLE owners (
    id TEXT PRIMARY KEY,
    tenant_id TEXT NOT NULL UNIQUE REFERENCES tenants(id) ON DELETE CASCADE,
    display_name TEXT NOT NULL,
    created_at TEXT NOT NULL
);

CREATE TABLE initialization_tokens (
    id TEXT PRIMARY KEY,
    token_hash TEXT NOT NULL UNIQUE,
    expires_at TEXT NOT NULL,
    used_at TEXT,
    created_at TEXT NOT NULL
);

CREATE TABLE passkeys (
    id TEXT PRIMARY KEY,
    owner_id TEXT NOT NULL REFERENCES owners(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    credential_json TEXT NOT NULL,
    created_at TEXT NOT NULL,
    last_used_at TEXT,
    revoked_at TEXT
);
CREATE UNIQUE INDEX passkeys_credential_idx ON passkeys(credential_json);

CREATE TABLE ceremonies (
    id TEXT PRIMARY KEY,
    kind TEXT NOT NULL,
    state_json TEXT NOT NULL,
    expires_at TEXT NOT NULL,
    consumed_at TEXT,
    created_at TEXT NOT NULL
);

CREATE TABLE login_sessions (
    id TEXT PRIMARY KEY,
    owner_id TEXT NOT NULL REFERENCES owners(id) ON DELETE CASCADE,
    token_hash TEXT NOT NULL UNIQUE,
    csrf_token TEXT NOT NULL,
    created_at TEXT NOT NULL,
    last_seen_at TEXT NOT NULL,
    expires_at TEXT NOT NULL,
    revoked_at TEXT
);

CREATE TABLE recovery_batches (
    id TEXT PRIMARY KEY,
    owner_id TEXT NOT NULL REFERENCES owners(id) ON DELETE CASCADE,
    created_at TEXT NOT NULL,
    revoked_at TEXT
);
CREATE TABLE recovery_codes (
    id TEXT PRIMARY KEY,
    batch_id TEXT NOT NULL REFERENCES recovery_batches(id) ON DELETE CASCADE,
    code_hash TEXT NOT NULL UNIQUE,
    used_at TEXT
);

CREATE TABLE recovery_states (
    id TEXT PRIMARY KEY,
    owner_id TEXT NOT NULL REFERENCES owners(id) ON DELETE CASCADE,
    token_hash TEXT NOT NULL UNIQUE,
    expires_at TEXT NOT NULL,
    used_at TEXT
);

CREATE TABLE model_providers (
    id TEXT PRIMARY KEY,
    owner_id TEXT NOT NULL REFERENCES owners(id) ON DELETE CASCADE,
    kind TEXT NOT NULL,
    display_name TEXT NOT NULL,
    base_url TEXT NOT NULL,
    api_key_ciphertext BLOB,
    api_key_fingerprint TEXT,
    api_key_preview TEXT,
    models_json TEXT NOT NULL DEFAULT '[]',
    enabled INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE UNIQUE INDEX model_provider_name_idx ON model_providers(owner_id, display_name);
