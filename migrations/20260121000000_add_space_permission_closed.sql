-- Migration: add_space_permission_closed
-- Date: 2026-01-21
-- Purpose: Introduce 'Closed' semantics for space permission. This value is
-- represented in serialized `space_info` metadata (numeric value 2). No direct
-- database schema changes are required because `space_permission` is stored
-- within JSON/extra metadata for views in Collab CRDT data (not in a database table).
-- 
-- NOTE: View metadata (including `extra` field with `is_space` and `space_permission`)
-- is stored in the `af_collab.blob` column as CRDT binary data, not in a separate `af_view` table.
-- Therefore, this migration is a no-op placeholder to document the semantic change.
-- 
-- If backfilling is needed, it should be done through application code that:
-- 1. Reads Collab folder data from `af_collab` where partition_key = 3 (folder)
-- 2. Decodes the CRDT blob to access view metadata
-- 3. Updates the `extra` JSON field for views where `is_space = true` and `space_permission` is missing
-- 4. Re-encodes and saves the updated Collab blob
--
-- This migration intentionally contains no SQL statements that modify the database schema or data.

-- End of migration.


