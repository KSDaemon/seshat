-- V4: FTS5 full-text search index for convention descriptions
-- and per-file convention compliance counter.

-- Standalone FTS5 virtual table (not external content) to avoid sync complexity.
-- Stores description + node_id + detector_name for joining back to nodes table.
CREATE VIRTUAL TABLE IF NOT EXISTS conventions_fts USING fts5(
    description,
    node_id UNINDEXED,
    detector_name
);

-- Per-file convention compliance count: how many conventions a file follows.
-- Used by golden files computation (US-007) to rank exemplar files.
ALTER TABLE files_ir ADD COLUMN convention_compliance_count INTEGER NOT NULL DEFAULT 0;
