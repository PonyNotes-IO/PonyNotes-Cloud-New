-- Migration: fix af_notification trigger to use consistent payload format
-- 修复通知触发器，使其与 af_user_channel 使用一致的 payload 格式

CREATE OR REPLACE FUNCTION notify_af_notification() RETURNS TRIGGER AS $$
DECLARE
    payload text;
BEGIN
    -- 使用与 af_user_channel 一致的格式：{'payload': row_to_json(NEW)}
    payload := json_build_object(
        'payload', row_to_json(NEW)
    )::text;

    PERFORM pg_notify('af_notification_channel', payload);
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;
