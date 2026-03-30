-- V3: Add last git commit date to file IR records
-- Used for convention trend detection (Rising/Stable/Declining).

ALTER TABLE files_ir ADD COLUMN last_commit_date INTEGER;
