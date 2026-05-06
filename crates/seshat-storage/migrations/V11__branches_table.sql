-- V11: branches table — per-branch metadata sentinel.
--
-- Tracks the last commit each branch was scanned at, so that startup
-- freshness checks (`seshat serve` / `seshat review`) can compare the
-- recorded HEAD against the live `git rev-parse HEAD` and trigger a
-- re-sync only when divergence is detected.
--
-- Additionally, decoupling branch enumeration from the `nodes` table
-- prevents a regression where branches without any nodes fail to appear
-- in `list_branches`.
CREATE TABLE IF NOT EXISTS branches (
    branch_id            TEXT PRIMARY KEY,
    last_scanned_commit  TEXT,
    last_scanned_at      INTEGER,
    snapshot_source      TEXT,
    created_at           INTEGER NOT NULL DEFAULT (unixepoch())
);
