-- V8: Add description_hash column for convention deduplication.
-- User-confirmed nodes get a SHA256 hash of their normalised description;
-- auto-detected nodes without the column (NULL) are unaffected until re-confirmed.
ALTER TABLE nodes ADD COLUMN description_hash TEXT DEFAULT NULL;
CREATE INDEX IF NOT EXISTS idx_nodes_description_hash ON nodes(description_hash);
