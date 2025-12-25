-- Migration: add af_notification table and trigger to notify on new notifications
CREATE TABLE IF NOT EXISTS af_notification (
  id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
  workspace_id UUID,
  notification_type TEXT NOT NULL,
  payload JSONB NOT NULL,
  recipient_uid BIGINT NULL,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  processed BOOLEAN NOT NULL DEFAULT FALSE
);

CREATE OR REPLACE FUNCTION notify_af_notification() RETURNS TRIGGER AS $$
BEGIN
  PERFORM pg_notify('af_notification_channel', row_to_json(NEW)::text);
  RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS af_notification_insert_trigger ON af_notification;
CREATE TRIGGER af_notification_insert_trigger
  AFTER INSERT ON af_notification
  FOR EACH ROW
  EXECUTE FUNCTION notify_af_notification();


