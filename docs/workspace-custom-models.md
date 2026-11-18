# 工作区问AI自定义模型

模型在原工作区的“设置 → 模型配置”管理；从问AI菜单点击“配置自定义模型”直接进入此页。设置弹窗目标为 1261 × 800，小窗口缩小并保留边距。

## 权限与使用范围

| 操作 | 原工作区管理员／拥有者 | 原工作区其他成员 | 配置者本人切换到其他工作区 | 其他工作区的其他成员 |
| --- | --- | --- | --- | --- |
| 在设置页查看配置 | 可以 | 可以，只读 | 不展示原工作区的管理卡片 | 不展示 |
| 新建、编辑、删除原工作区模型 | 可以 | 不可以 | 回原工作区管理 | 不可以 |
| 在问AI菜单选用 | 可以 | 可以 | 可以，本人可携带使用 | 不可以 |

沿用现有权限：`AFRole::Owner`、工作区 `owner_uid` 和认证后的 `supabase_admin` 有管理权。项目当前没有独立的工作区 Admin 枚举，不将普通 Member 当作管理员。配置者 `creator_uid` 由登录身份写入，编辑时不改变。

设置列表请求只取当前工作区；问AI列表另包含配置者自己的其他工作区模型。调用时重新验证当前工作区访问权，并检查“模型原工作区等于当前工作区，或配置者等于调用者”。删除模型后，新调用被拒绝。

## 接口与凭据

新增 `/api/ai/agent/custom-models/{workspace_id}`：GET 获取配置，POST 新建；`/{id}` 的 PUT、DELETE 编辑和删除。GET 的 `personal=true` 仅用于问AI菜单。响应 ID 带 `custom:` 前缀；调用 `/conversations/.../messages` 时提交 UUID 字段 `model.custom_model_id`。

成员拿到的是模型 ID 和非敏感配置。服务端从 `af_agent_custom_models` 读取并校验归属后才解密 API Key，再交给既有 WeKnora 对话执行链。修改地址或服务商时必须重新提供 Key，避免将已有凭据发往新地址。上下文、输出上限和思考能力沿用保存的配置。

API Key 使用 AES-256-GCM 加密，随机 nonce，并用模型 UUID 绑定认证数据。加密密钥由既有 GoTrue JWT secret 通过独立用途的 HMAC 派生。备份与恢复必须保留此 secret；轮换 JWT secret 前须用旧 secret 解密并重新加密模型凭据，不能直接丢弃旧 secret。

模型统一从“设置 → 模型配置 → 添加模型”管理，问AI菜单读取相同的工作区模型目录；不提供本机导入步骤。原本机旧配置保留在系统安全存储，继续按现有兼容路径使用，不自动上传或共享 Key。

Embedding、ReRank、视觉、语音与已有知识库模型继续使用原 WeKnora 配置及 UUID 绑定；`custom:` 模型不进入知识库 UUID 绑定。

## 发布与验证

2026-11-18 已部署测试服 Cloud `ponynotes/cloud:workspace-models-20261118`，新增 SQLx 迁移已应用。已用三个真实测试账号验证列表、管理权限和 Cloud 到 WeKnora 的模型 ID 派发。使用假 Key 和非商业测试端点，不涉及付费模型推理。

客户端定向回归覆盖菜单选择、设置跳转、实际渲染尺寸、成员只读、账号隔离和跨工作区刷新。新增服务端模块的 4 项测试覆盖使用范围、密钥加密、响应隐藏凭据及变更端点时的 Key 约束。

本机完整 Cloud 测试被原生依赖的 Visual Studio/CMake 生成器阻断。此前通过 `outputs/workspace-models-20261118/cloud-check/` 直接编译生产 `custom_models.rs` 并运行 4 项隔离测试，认证入口与 AppState 使用类型替身。现在已补齐 Linux release 构建、真实 PostgreSQL 迁移与三个账号的 API 集成验收；完整 Cloud 单测入口仍未完成。


### 测试服验收与恢复

发布前已拉取 origin，当前分支没有新增提交；保留另一个分支的 `9b49764f` 智能体目录提交及测试服已部署的模型删除修复。Linux release 构建与 WeKnora 库 36 项测试通过。完整 Cloud 单测依赖编译未完成，验收以三个账号的实际 API 权限矩阵与对话派发为依据，详情在根工作区 `deploy/workspace-models-20261118/README.md`。

备份：`/opt/ponynotes/backups/workspace-models-20261118-retry/`。回滚：`bash /opt/ponynotes/releases/test-workspace-models-20261118/rollback.sh`；新增模型表和记录保留。公网健康接口为 200，已有模型记录保留，临时验收数据已清理。
