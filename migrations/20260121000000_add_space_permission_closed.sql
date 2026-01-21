-- Migration: add_space_permission_closed
-- Date: 2026-01-21
-- Purpose: Introduce 'Closed' semantics for space permission. This value is
-- represented in serialized `space_info` metadata (numeric value 2). No direct
-- database schema changes are required because `space_permission` is stored
-- within JSON/extra metadata for views. This migration is a placeholder to
-- document the semantic change and can be extended to backfill metadata if
-- necessary in your deployment.

-- Example backfill (uncomment and adapt if you need to convert existing records):
-- UPDATE af_view
-- SET extra = jsonb_set(extra, '{space_permission}', '2'::jsonb)
-- WHERE (extra->>'space_permission') IS NULL AND (extra->>'is_space') = 'true';
-- Backfill SQL (safe): set space_permission = 2 ("Closed") only for views whose
-- `extra` JSON indicates they are a space (`"is_space": true`) and which do not
-- already have `space_permission` set. This avoids overwriting explicit values.
-- NOTE: `extra` is expected to be JSON (text) that can be cast to jsonb. Rows
-- with invalid JSON in `extra` are skipped to avoid errors.

-- 1) Update rows where extra is valid JSON, is_space = true, and space_permission is missing.
UPDATE af_view
SET extra = jsonb_set(extra::jsonb, '{space_permission}', '2'::jsonb)::text
WHERE extra IS NOT NULL
  AND (extra::jsonb ->> 'is_space') = 'true'
  AND (extra::jsonb ->> 'space_permission') IS NULL;

-- 2) (Optional) If you want to initialize rows that currently have NULL extra but
-- should be spaces, you can uncomment and adapt the following block. It creates a
-- minimal extra JSON containing is_space=true and space_permission=2. Use with care.
-- INSERT/UPDATE of NULL extras is commented out by default because detection of
-- "is_space" when extra is NULL may require additional business rules.

-- UPDATE af_view
-- SET extra = ('{"is_space": true, "space_permission": 2}')::text
-- WHERE extra IS NULL
--   AND /* add your condition to identify these rows, e.g. view names or other columns */ false;

-- End of migration.


