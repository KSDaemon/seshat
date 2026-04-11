-- Add ir_schema_version column to files_ir so the scanner can detect stale
-- blobs without attempting to deserialize them. Existing rows get version 0
-- (intentionally invalid) to force a full re-parse on the next scan.
ALTER TABLE files_ir ADD COLUMN ir_schema_version INTEGER NOT NULL DEFAULT 0;
