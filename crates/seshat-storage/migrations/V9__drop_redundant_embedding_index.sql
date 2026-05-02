-- V9: Drop redundant single-column idx_code_embeddings_branch.
-- The compound index idx_code_embeddings_branch_file(branch_id, file_path)
-- already covers branch_id lookups via leftmost prefix, making this index
-- unnecessary and wasteful for write performance.
DROP INDEX IF EXISTS idx_code_embeddings_branch;
