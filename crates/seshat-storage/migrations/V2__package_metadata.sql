-- V2: Package registry metadata cache
-- Stores categories, keywords, and description fetched from package registries
-- (crates.io, npm, PyPI) to enrich dependency classification.

CREATE TABLE IF NOT EXISTS package_metadata (
    name        TEXT    NOT NULL,
    registry    TEXT    NOT NULL,
    categories  TEXT    NOT NULL DEFAULT '[]',   -- JSON array of strings
    keywords    TEXT    NOT NULL DEFAULT '[]',   -- JSON array of strings
    description TEXT,                            -- nullable
    fetched_at  INTEGER NOT NULL,                -- Unix timestamp
    PRIMARY KEY (name, registry)
);

CREATE INDEX IF NOT EXISTS idx_package_metadata_registry ON package_metadata(registry);
CREATE INDEX IF NOT EXISTS idx_package_metadata_fetched_at ON package_metadata(fetched_at);
