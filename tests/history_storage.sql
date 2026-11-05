-- Run against an empty disposable PostgreSQL database with psql -v ON_ERROR_STOP=1.
BEGIN;
CREATE TABLE af_user(uid BIGINT PRIMARY KEY);
CREATE TABLE af_workspace(workspace_id UUID PRIMARY KEY, owner_uid BIGINT);
CREATE TABLE af_subscription_plans(plan_code TEXT PRIMARY KEY, version_history_days INTEGER);
INSERT INTO af_subscription_plans VALUES ('mfb',-1),('standard',-1),('hiclass',-1),('profersor',-1);
INSERT INTO af_user VALUES(1);
INSERT INTO af_workspace VALUES('00000000-0000-0000-0000-000000000001',1),('00000000-0000-0000-0000-000000000002',1);
CREATE EXTENSION IF NOT EXISTS "uuid-ossp";
\ir ../migrations/20261105000000_manual_document_versions.sql

DO $$
DECLARE
  id UUID;
  n BIGINT;
BEGIN
  IF (SELECT version_history_days FROM af_subscription_plans WHERE plan_code='mfb') <> 0
    OR (SELECT version_history_days FROM af_subscription_plans WHERE plan_code='standard') <> 7
    OR (SELECT version_history_days FROM af_subscription_plans WHERE plan_code='hiclass') <> 30
    OR (SELECT version_history_days FROM af_subscription_plans WHERE plan_code='profersor') <> 30 THEN
    RAISE EXCEPTION 'incorrect plan retention';
  END IF;

  INSERT INTO af_document_version(workspace_id,oid,created_by,document,expires_at)
  VALUES('00000000-0000-0000-0000-000000000001','00000000-0000-0000-0000-000000000010',1,
    convert_to('{"page_id":"root","blocks":{},"meta":{"children_map":{}}}', 'UTF8'),NOW()+make_interval(days=>7))
  RETURNING snapshot_id INTO id;
  SELECT COUNT(*) INTO n FROM af_document_version
    WHERE workspace_id='00000000-0000-0000-0000-000000000002' AND snapshot_id=id;
  IF n <> 0 THEN RAISE EXCEPTION 'cross workspace version leak'; END IF;

  INSERT INTO af_document_version(workspace_id,oid,created_by,document,created_at,expires_at)
  SELECT '00000000-0000-0000-0000-000000000001','00000000-0000-0000-0000-000000000010',1,
    convert_to('{}','UTF8'),NOW()-make_interval(days=>d),NOW()+make_interval(days=>30-d)
    FROM generate_series(8,9) d;
  SELECT COUNT(*) INTO n FROM af_document_version
    WHERE workspace_id='00000000-0000-0000-0000-000000000001'
      AND expires_at>NOW() AND created_at>=NOW()-make_interval(days=>7);
  IF n <> 1 THEN RAISE EXCEPTION '7 day filtering failed'; END IF;
  SELECT COUNT(*) INTO n FROM af_document_version
    WHERE expires_at>NOW() AND created_at>=NOW()-make_interval(days=>30);
  IF n <> 3 THEN RAISE EXCEPTION '30 day filtering failed'; END IF;

  UPDATE af_document_version SET expires_at=NOW()-INTERVAL '1 second' WHERE snapshot_id=id;
  DELETE FROM af_document_version WHERE expires_at<=NOW();
  IF EXISTS(SELECT 1 FROM af_document_version WHERE snapshot_id=id) THEN
    RAISE EXCEPTION 'expiration cleanup failed';
  END IF;
  DELETE FROM af_user WHERE uid=1;
  IF EXISTS(SELECT 1 FROM af_document_version WHERE created_by IS NOT NULL) THEN
    RAISE EXCEPTION 'creator deletion should preserve workspace history';
  END IF;
  DELETE FROM af_workspace;
  IF EXISTS(SELECT 1 FROM af_document_version) THEN RAISE EXCEPTION 'workspace cascade failed'; END IF;
END $$;
ROLLBACK;
