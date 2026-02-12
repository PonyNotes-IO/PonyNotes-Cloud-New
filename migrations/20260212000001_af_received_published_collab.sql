-- 用于跟踪用户接收的发布文档
-- 发布者可以在自己的发布菜单中看到自己发布的文档
-- 接收者可以在自己的发布菜单中看到接收到的发布文档
CREATE TABLE IF NOT EXISTS af_received_published_collab (
    -- 接收者的用户ID
    received_by BIGINT NOT NULL REFERENCES af_user(uid) ON DELETE CASCADE,
    -- 发布的文档视图ID（关联 af_published_collab.view_id）
    published_view_id UUID NOT NULL,
    -- 接收者工作区ID（复制后的文档所在的工作区）
    workspace_id UUID NOT NULL,
    -- 复制后的文档视图ID
    view_id UUID NOT NULL,
    -- 发布者的用户ID（用于显示发布者信息）
    published_by BIGINT NOT NULL REFERENCES af_user(uid) ON DELETE CASCADE,
    -- 发布时间
    published_at TIMESTAMP WITH TIME ZONE NOT NULL,
    -- 接收时间
    received_at TIMESTAMP WITH TIME ZONE DEFAULT CURRENT_TIMESTAMP,
    -- 是否为只读模式（发布的文档对接收者默认只读）
    is_readonly BOOLEAN NOT NULL DEFAULT TRUE,

    PRIMARY KEY (received_by, published_view_id)
);

-- 索引优化
CREATE INDEX IF NOT EXISTS idx_received_by ON af_received_published_collab(received_by);
CREATE INDEX IF NOT EXISTS idx_published_view_id ON af_received_published_collab(published_view_id);
CREATE INDEX IF NOT EXISTS idx_received_workspace_id ON af_received_published_collab(workspace_id);
CREATE INDEX IF NOT EXISTS idx_received_view_id ON af_received_published_collab(view_id);

-- 外键约束
ALTER TABLE af_received_published_collab
ADD CONSTRAINT fk_received_published_view_id
FOREIGN KEY (published_view_id) REFERENCES af_published_collab(view_id) ON DELETE CASCADE;

ALTER TABLE af_received_published_collab
ADD CONSTRAINT fk_received_workspace_id
FOREIGN KEY (workspace_id) REFERENCES af_workspace(workspace_id) ON DELETE CASCADE;
