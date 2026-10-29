CREATE SCHEMA IF NOT EXISTS auth;
CREATE EXTENSION IF NOT EXISTS "uuid-ossp" WITH SCHEMA public;

CREATE TABLE IF NOT EXISTS public.wechat_app_fulfillment (
    order_no varchar(32) PRIMARY KEY,
    subscription_id bigint,
    created_at timestamptz NOT NULL DEFAULT now()
);
