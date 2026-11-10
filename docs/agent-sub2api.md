# Agent 内部模型接入 sub2api

用户首次进入 Agent 时，cloud 自动为该小马用户创建独立 sub2api 账户和专属 API Key，并保存一对一绑定。用户在模型菜单中直接选择「内部模型」，实际请求使用其自己的 sub2api Key 计费。原有自定义模型仍可独立使用。

## 配置服务端自动开通

1. 在服务端私有环境文件中设置以下变量，再按项目原有流程生成 `.env` 并发布 cloud。Docker Compose 已透传这些变量；直接运行 cloud 时也需加载它们。

   ```dotenv
   PONYNOTES_SUB2API_URL=https://your-sub2api.example
   PONYNOTES_SUB2API_ADMIN_KEY=<sub2api admin API key>
   # sub2api 0.2.19 的权限管理密钥不支持创建用户；此版本留空 ADMIN_KEY，配置：
   PONYNOTES_SUB2API_ADMIN_EMAIL=<sub2api administrator email>
   PONYNOTES_SUB2API_ADMIN_PASSWORD=<sub2api administrator password>
   PONYNOTES_SUB2API_PROVISION_SECRET=<at least 32 bytes of random secret>
   PONYNOTES_SUB2API_GROUP_ID=<sub2api model group ID>
   PONYNOTES_CREDITS_PER_USD=100
   ```

   地址可带 `/v1`。生产环境使用 HTTPS，或仅通过受保护的内部网络访问 sub2api。管理凭据只在 cloud 服务端配置，不放入客户端、执行器或 Git。`ADMIN_KEY` 非空时使用管理 Key，否则使用管理员账号正常登录，管理会话仅缓存在内存中，并按有效期自动刷新。

2. 应用迁移 `20261106000000_agent_sub2api_accounts.sql` 和 `20261106010000_agent_auto_provision.sql`。前者创建账号映射表，后者增加开通恢复标识并允许创建过程暂存待开通记录。旧 AI 次数配额保持原逻辑，新查询使用 SQLx 运行时参数绑定，不新增离线查询缓存。

3. `GROUP_ID` 指定新用户专属 Key 绑定的标准余额计费分组。sub2api 的订阅分组要求已有有效订阅，不用于这里的零余额自动开通。首次加载内部模型时，cloud 自动完成以下流程：

   - 为小马 `af_user.uid` 持久化唯一的 `provision_id`。
   - 通过 sub2api 管理接口创建普通用户，用户名使用小马用户名，备注保留小马 UID；初始余额为 0，并授予指定分组。
   - 以该用户身份认证，通过用户 Key 接口创建其专属 Key。
   - 在 `af_agent_accounts` 中保存 sub2api 用户 ID 和 Key ID，后续请求复用该绑定。

   `uid`、sub2api 用户 ID 和 Key ID 分别有唯一约束；同一用户的创建过程使用数据库行锁串行执行。用户的 sub2api 余额、套餐限额和用量明细都独立计算。余额充值在该用户的 sub2api 账户中进行。

   用户名只作运维展示，允许重名；一对一绑定和中断恢复始终使用 ID、`provision_id` 及内部邮箱标识。

   sub2api 当前网关对 `/v1/models` 也执行余额检查；新建零余额账户可能先显示充值提示，充值后重新打开模型菜单即可。自动开通不会赠送余额或绕过网关检查。

4. 妥善保存 `PROVISION_SECRET` 和 cloud 数据库。它用于按开通标识分别派生高强度登录密码及 API Key，二者使用不同的 HMAC 域。生成的账户使用唯一的内部邮箱标识；小马的手机号、邮箱及密码不参与 sub2api 密码生成。cloud 数据库只保存标识和 ID，凭据仅在请求时使用。

   开通请求超时、服务重启或保存 ID 的事务中断后，后续请求通过稳定标识找回同一 sub2api 用户和 Key。已有账号的密码不会被自动重置；已完成的绑定继续复用，已停用或已删除的 Key 会明确报错。既有完整绑定无需新增自动开通配置即可继续工作。

   所有 cloud 实例使用同一份开通密钥；仍有待完成的开通记录时不要更换它，否则无法恢复该用户的登录及专属 Key。已完成的绑定从 sub2api 读取其原 Key，不受密钥更换影响。

## 请求与积分

- 登录凭证只用于 cloud 的 `GET /api/ai/agent/models`、`GET /api/ai/agent/usage`、`POST /api/ai/agent/session`。
- 模型从绑定 Key 的 sub2api `GET /v1/models` 动态读取，不复用旧 Chat 的固定模型列表。
- 执行器仅收到 cloud 签发的随机 Agent 会话凭证，通过 `/api/ai/agent/chat/completions` 请求。它既不持有登录 JWT，也不持有 sub2api Key。会话有效期 24 小时，后续任务开始前会在临近到期时刷新；切换模型、断开和退出账号时撤销。撤销失败会显示提示，服务端到期兜底。单个连续运行超过 24 小时的任务需要重新连接。
- cloud 按已认证用户查找绑定，覆盖请求中的 `user` 为小马 uid，只转发模型请求。工具调用产生的后续轮次、重试和上下文压缩仍经过相同代理。
- **sub2api 是唯一计费账本。** 每轮请求的实际费用、模型倍率、余额/套餐/Key 限额都由 sub2api 结算，cloud 不重复扣减旧 Chat 次数。用量明细在 sub2api 对应用户和 Key 的使用记录中查看。
- 标准余额分组在 Key 有额度或速率限制时，从管理接口读取最新钱包余额；有 Key 总额度时取钱包余额与 Key 剩余额度的较小值。其他模式沿用上游返回的可用额度，`remaining = -1` 显示「积分不限额」。积分按 `PONYNOTES_CREDITS_PER_USD` 换算，调整比例不改变 sub2api 的 USD 账本。
- Agent 连接期间约每 5 秒刷新一次积分，任务结束和切换模型后也会刷新；网络延迟会增加显示延迟。内部积分查询失败显示重试入口，不沿用旧余额。自定义模型沿用其原有地址、凭据和余额展示逻辑。

## 发布验收

代码回归使用 `cargo test -p appflowy-cloud --lib api::agent`。其中数据库测试需要 `DATABASE_URL` 指向具有创建测试数据库权限的 PostgreSQL 测试实例；它会验证并发开通、重试恢复及唯一绑定，不调用真实 sub2api。

服务端需要 PostgreSQL、Redis，以及支持以下接口的 sub2api 版本：管理员 `GET/POST /api/v1/admin/users`、`GET /api/v1/admin/users/:id` 和 `GET /api/v1/admin/users/:id/api-keys`（`x-api-key`，分页 `data.items`/`data.pages`）；用户 `POST /api/v1/auth/login` 和 `POST /api/v1/keys`（用户 JWT，支持 `custom_key`）；网关 `GET /v1/models`、`GET /v1/usage` 和 `POST /v1/chat/completions`。使用受限管理 Key 时，读取钱包余额还需 `users.balance.read` 权限。契约依据 upstream `Wei-Shaw/sub2api` 的路由及类型核对，发布前需对实际部署版本联调。

自动建 Key 使用 sub2api 的正常用户认证流程；采用管理账号模式时，管理员也通过正常登录接口认证。部署的登录限流、交互验证码、强制双因素或仅管理员后台模式可能阻止此流程；此时会保留开通记录并返回失败，需在 sub2api 侧提供受支持的非交互服务认证能力后重试，不改变现有登录安全策略。

使用两个尚未开通的小马测试账号，验证首次打开 Agent 后分别只有一个 sub2api 用户及 Key；并发打开多个文档和中断后重试仍复用原账户。给其中一个 sub2api 测试账户充值后，验证内部模型任务及工具调用、其 Key 的实际用量和积分显示，并确认另一账号的余额不变。余额不足应由网关拒绝，退出后旧 Agent 会话应无法继续请求。真实模型验收会产生该测试账号的实际费用。
