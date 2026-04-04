-- V6: Code embeddings table for vector search
-- Stores per-item embeddings generated during seshat scan.
-- When the [embedding] config section is absent, this table remains empty (zero overhead).

CREATE TABLE IF NOT EXISTS code_embeddings (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    branch_id   TEXT    NOT NULL,
    file_path   TEXT    NOT NULL,
    item_name   TEXT    NOT NULL,
    item_kind   TEXT    NOT NULL,   -- 'function', 'type', or 'export'
    embedding   BLOB    NOT NULL,   -- raw f32 bytes (little-endian)
    UNIQUE(branch_id, file_path, item_name, item_kind)
);

CREATE INDEX IF NOT EXISTS idx_code_embeddings_branch
    ON code_embeddings(branch_id);

CREATE INDEX IF NOT EXISTS idx_code_embeddings_branch_file
    ON code_embeddings(branch_id, file_path);
