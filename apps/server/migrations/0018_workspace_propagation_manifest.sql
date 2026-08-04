-- janus-module: workspace-sync
-- Persist the three-way baseline as a manifest instead of copying every
-- Session file into a second filesystem tree. Existing rows are migrated
-- lazily from their legacy baseline directory by the Workspace interface.
ALTER TABLE propagation_links ADD COLUMN baseline_manifest_json TEXT;
