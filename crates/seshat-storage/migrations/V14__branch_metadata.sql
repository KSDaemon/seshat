-- V14: branch_metadata table — per-branch key/value storage.
--
-- Moves workspace_crates (and any future per-branch scoped metadata) off the
-- global `repo_metadata` slot, where every branch's scan clobbered the
-- previous branch's value. Keyed by `(branch_id, key)` so different branches
-- coexist without cross-contamination.
--
-- The FK + ON DELETE CASCADE means deleting a branch (via
-- `BranchRepository::delete_branch`) automatically removes its metadata —
-- callers do not have to clean it up explicitly. SQLite enforces this only
-- when `PRAGMA foreign_keys = ON`, which `Database::open` already sets.
-- The composite PRIMARY KEY (branch_id, key) is a B-tree whose leftmost
-- column is `branch_id`, so it already serves point and range lookups by
-- `branch_id` alone; no secondary index is needed.
CREATE TABLE IF NOT EXISTS branch_metadata (
    branch_id  TEXT    NOT NULL,
    key        TEXT    NOT NULL,
    value      TEXT    NOT NULL,
    updated_at INTEGER NOT NULL DEFAULT (unixepoch()),
    PRIMARY KEY (branch_id, key),
    FOREIGN KEY (branch_id) REFERENCES branches(branch_id) ON DELETE CASCADE
);
