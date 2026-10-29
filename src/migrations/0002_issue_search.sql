-- Search compares against text lowercased by the application, because MariaDB's LOWER()
-- folds Unicode while SQLite's only folds ASCII. New and edited issues fill it in Rust;
-- this backfill only lowercases what both databases agree on for existing rows.
ALTER TABLE issues ADD COLUMN search_text TEXT NOT NULL DEFAULT '';
UPDATE issues SET search_text = LOWER(title || ' ' || body);

-- The board and status filters read one status at a time.
CREATE INDEX idx_issues_status ON issues (status, due_at);
