-- V12: decisions table.
-- Single source of truth for all user-recorded knowledge:
--   state='approved' / 'rejected' / 'partial' — TUI review of auto-detected
--   state='recorded'                          — explicit decision via MCP record_decision
--
-- Project-wide (NOT branch-scoped): one row per `description_hash`. UPSERT
-- on conflict replaces the existing row. `decided_on_branch` is recorded
-- for audit purposes only and does not participate in lookup.
CREATE TABLE IF NOT EXISTS decisions (
    description_hash     TEXT NOT NULL PRIMARY KEY,
    description          TEXT NOT NULL,
    state                TEXT NOT NULL CHECK (state IN ('approved','rejected','partial','recorded')),
    nature               TEXT NOT NULL CHECK (nature IN ('convention','decision','preference','fact')),
    weight               TEXT NOT NULL CHECK (weight IN ('rule','strong')),
    category             TEXT,
    reason               TEXT,
    examples             TEXT,                  -- JSON: [{file, line, end_line, snippet}, ...]
    decided_on_branch    TEXT NOT NULL,
    decided_at           INTEGER NOT NULL,
    updated_at           INTEGER NOT NULL DEFAULT (unixepoch())
);

CREATE INDEX IF NOT EXISTS idx_decisions_state             ON decisions(state);
CREATE INDEX IF NOT EXISTS idx_decisions_decided_on_branch ON decisions(decided_on_branch);
