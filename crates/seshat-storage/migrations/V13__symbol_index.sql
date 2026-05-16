-- V13: symbol-index tables — O(log N) per-symbol lookup for query_code_pattern.
--
-- `symbol_definitions` holds one row per Function / TypeDef / Export emitted by
-- the IR.  `kind` is restricted to those three values; `snippet` already holds
-- the truncated definition snippet so reads do not have to JOIN back to
-- `files_ir` and deserialize the IR blob.
--
-- `symbol_imports` holds one row per concrete-named import per file.
-- Wildcard imports (`use foo::*`, `from foo import *`, `import * as foo from …`)
-- are not stored here — they cannot be attributed to a single defining symbol.
--
-- The Rust-side backfill in `Database::open` reads existing `files_ir` rows,
-- deserializes `ir_data`, and populates these tables.  Backfill is gated on
-- "symbol_definitions empty AND files_ir non-empty" so re-opening a DB that
-- has already been populated is a no-op.
CREATE TABLE IF NOT EXISTS symbol_definitions (
    branch_id   TEXT    NOT NULL,
    symbol_name TEXT    NOT NULL,
    file_path   TEXT    NOT NULL,
    line        INTEGER NOT NULL,
    end_line    INTEGER NOT NULL,
    kind        TEXT    NOT NULL CHECK (kind IN ('function','type','export')),
    is_public   INTEGER NOT NULL,
    snippet     TEXT    NOT NULL
);

CREATE TABLE IF NOT EXISTS symbol_imports (
    branch_id     TEXT NOT NULL,
    imported_name TEXT NOT NULL,
    importer_file TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_symbol_definitions_branch_name
    ON symbol_definitions (branch_id, symbol_name);

CREATE INDEX IF NOT EXISTS idx_symbol_imports_branch_name
    ON symbol_imports (branch_id, imported_name);
