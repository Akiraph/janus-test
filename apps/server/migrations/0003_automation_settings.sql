CREATE TABLE automation_settings (
    owner_id TEXT PRIMARY KEY REFERENCES owners(id) ON DELETE CASCADE,
    model_provider_id TEXT,
    model_upstream_id TEXT,
    updated_at TEXT NOT NULL
);
