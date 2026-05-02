ALTER TABLE code_embeddings ADD COLUMN updated_at TEXT NOT NULL DEFAULT (datetime('now'));
