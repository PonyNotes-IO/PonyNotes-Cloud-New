-- 创建发布接收记录表
-- 用于记录哪些用户接收了发布的文档
CREATE TABLE IF NOT EXISTS af_published_collab_receive (
    id BIGSERIAL PRIMARY KEY,
    view_id UUID NOT NULL,           -- 接收后的本地视图ID
    published_view_id UUID NOT NULL, -- 发布文档的原始ID
    received_by BIGINT NOT NULL,     -- 接收者用户ID
    publisher_uid BIGINT NOT NULL,   -- 发布者用户ID
    workspace_id UUID NOT NULL,     -- 接收者的工作区ID
    publish_name VARCHAR(255),       -- 发布名称
    name VARCHAR(255),               -- 文档名称
    created_at TIMESTAMP WITH TIME ZONE DEFAULT CURRENT_TIMESTAMP,
    UNIQUE(view_id, received_by)
);

-- 创建索引以加快查询
CREATE INDEX idx_published_receive_received_by ON af_published_collab_receive(received_by);
CREATE INDEX idx_published_receive_publisher ON af_published_collab_receive(publisher_uid);
CREATE INDEX idx_published_receive_workspace ON af_published_collab_receive(workspace_id);

COMMENT ON TABLE af_published_collab_receive IS '记录用户接收的发布文档，用于在发布菜单中显示"我接收的发布"';
