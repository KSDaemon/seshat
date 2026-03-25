-- V1: Initial schema for Seshat knowledge graph storage
-- Creates core tables: nodes, edges, files_ir, metadata

-- Knowledge graph nodes
CREATE TABLE IF NOT EXISTS nodes (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    branch_id   TEXT    NOT NULL,
    nature      TEXT    NOT NULL,
    weight      TEXT    NOT NULL,
    confidence  REAL    NOT NULL,
    adoption_count INTEGER NOT NULL DEFAULT 0,
    total_count    INTEGER NOT NULL DEFAULT 0,
    description TEXT    NOT NULL DEFAULT '',
    ext_data    TEXT            -- JSON, nullable
);

-- Knowledge graph edges (relationships between nodes)
CREATE TABLE IF NOT EXISTS edges (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    source_id   INTEGER NOT NULL REFERENCES nodes(id),
    target_id   INTEGER NOT NULL REFERENCES nodes(id),
    edge_type   TEXT    NOT NULL,
    branch_id   TEXT    NOT NULL,
    weight      REAL    NOT NULL DEFAULT 1.0,
    metadata    TEXT            -- JSON, nullable
);

-- Intermediate representation cache for scanned files
CREATE TABLE IF NOT EXISTS files_ir (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    branch_id    TEXT    NOT NULL,
    file_path    TEXT    NOT NULL,
    language     TEXT    NOT NULL,
    content_hash TEXT    NOT NULL,
    ir_data      BLOB   NOT NULL,
    updated_at   TEXT    NOT NULL DEFAULT (datetime('now')),
    UNIQUE(branch_id, file_path)
);

-- Key-value metadata store (e.g., current branch)
CREATE TABLE IF NOT EXISTS metadata (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

-- Indexes for efficient querying
CREATE INDEX IF NOT EXISTS idx_nodes_branch_id   ON nodes(branch_id);
CREATE INDEX IF NOT EXISTS idx_nodes_nature       ON nodes(nature);
CREATE INDEX IF NOT EXISTS idx_edges_source_id    ON edges(source_id);
CREATE INDEX IF NOT EXISTS idx_edges_target_id    ON edges(target_id);
CREATE INDEX IF NOT EXISTS idx_files_ir_branch_path ON files_ir(branch_id, file_path);
