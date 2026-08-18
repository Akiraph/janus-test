ALTER TABLE github_credentials
    ADD COLUMN automation_enabled INTEGER NOT NULL DEFAULT 0;
