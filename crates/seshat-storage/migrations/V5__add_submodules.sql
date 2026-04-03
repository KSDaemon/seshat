-- Submodule tracking: links submodule mount paths to their dedicated DBs.
CREATE TABLE IF NOT EXISTS submodules (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    relative_path TEXT    NOT NULL UNIQUE,  -- mount path relative to repo root (e.g. "vendor/lib")
    name          TEXT    NOT NULL,         -- human-readable submodule name (basename of path)
    db_path       TEXT    NOT NULL,         -- absolute path to the submodule's dedicated .db file
    commit_hash   TEXT,                     -- current HEAD of the submodule (for change detection)
    created_at    TEXT    NOT NULL DEFAULT (datetime('now')),
    updated_at    TEXT    NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS idx_submodules_relative_path ON submodules(relative_path);

-- Key-value store for repo-level metadata (project_name, last_scan_time, etc.).
CREATE TABLE IF NOT EXISTS repo_metadata (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
