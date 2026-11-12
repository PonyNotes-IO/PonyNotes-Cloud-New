# 小马 Agent 的服务端执行链路

客户端保留“问 AI”文档、模型选择、积分、附件、笔记工具、写入审批和修改前后预览。Agent 循环由 WeKnora Go 服务执行；Flutter 不再启动 Node/pi，也不再提供工作目录、系统文件工具、命令或可执行 Skills。产品界面、提示词身份、回答和错误统一使用“小马 Agent”。

## 请求和工具执行

```text
Flutter → Cloud /api/ai/agent/conversations/{workspace_id}/{document_id}
        → WeKnora agent-chat（模型、工具循环、历史、压缩、引用）
        → Cloud /api/ai/agent/note-tools（Streamable HTTP MCP）
        → Redis 待处理调用 → 当前 Flutter 客户端
        → 原 McpTools / AgentNoteBridge → 笔记和数据库业务服务
```

客户端注册原有笔记工具定义，通过已登录的 Cloud API 领取操作。写入在 Flutter 的业务边界检查当前账号、工作区、工具开关、参数及审批；审批期间页面变化会拒绝继续修改。保留逐次确认、自动审批和完全开放三种模式。停止、断线、退出或连接被其他窗口替换时撤销待审批动作；已经交给业务服务的操作应查询实际结果，不能自动重试。

MCP 的随机凭据只在 Cloud、Redis 和 WeKnora 的加密凭据存储之间流转，不进入客户端对话。客户端每秒刷新连接租约，租约 15 秒后失效；每次注册变更授权代次，使旧调用不能恢复执行。工具请求不自动重放，客户端按调用 ID 执行一次。

## 发布前准备

1. 先发布 Cloud，执行 `migrations/20261111000000_weknora_agent_sessions.sql`，再发布客户端。新查询使用运行时 SQLx API，不需要增补 `.sqlx` 查询缓存。
2. 保留既有 `PONYNOTES_WEKNORA_ENABLED=true`、`PONYNOTES_WEKNORA_URL` 和用户同步映射。适配依据为 WeKnora 提交 `26723dca3ba555c2baaed83e6e8276664b8fa395`；已有更旧部署需验证模型凭据子资源、MCP metadata/refresh、会话和继续流接口。
3. WeKnora 必须能访问 Cloud 的 `/api/ai/agent/chat/completions` 和 `/api/ai/agent/note-tools`。默认 Base URL 为 `${PONYNOTES_WEKNORA_CLOUD_URL}/api/ai/agent`；网络不同时设置 `PONYNOTES_WEKNORA_MODEL_BASE_URL` 为同一 Cloud 的 Agent Base URL。只在 WeKnora 的 `SSRF_WHITELIST_EXTRA` 添加实际 Cloud 主机，不关闭 SSRF 检查。
4. Cloud 根据当前用户的内部模型列表建立独立模型配置，模型请求仍经过 Cloud → 用户自己的 sub2api Key，由 sub2api 计费。WeKnora 里存储的是有期限的 Cloud 模型令牌，每轮提交时更新；客户端不需要获得 sub2api Key。自定义模型的凭据会保存到用户自己的 WeKnora 模型配置中，部署应配置其加密存储；模型地址必须可由服务端访问，本机 loopback 模型不再能从远端引擎直连。
5. 联网查询需要在对应用户的 WeKnora 空间配置搜索服务；客户端的“联网”开关只决定本轮是否开放搜索。

## 会话和资料范围

`af_weknora_agent_sessions` 按用户、工作区、问 AI 文档保存模型、智能体及会话映射。会话记录由 WeKnora 保存，客户端通过 GET 加载历史，断流后用已有 assistant message ID 恢复，不重新发问题。运行中可补充指令或排队，停止接口由服务端传播取消。

Cloud 根据当前可见页面筛选托管知识副本，只把授权文档 ID 交给 Agent，不开放整个知识库。若已有会话记录的资料范围出现减少，后续请求使用新会话，避免撤权后的旧上下文进入新一轮；旧服务端会话保留但不再向当前客户端返回。知识副本仍沿用原 Worker 的约五分钟扫描周期，刚编辑的内容可用客户端 `get_page` 等工具读取。

旧 pi 原生历史仍留在原本的本地应用数据目录，没有删除，也没有冒充新引擎的原生会话导入。原有 Chat 文档与 Chat 执行入口不变。退出客户端后，本机笔记工具不可执行。

## 本次验证

- Flutter 的 Agent 测试覆盖事件分片、流断开、模型与积分、笔记工具注册、拒绝/批准写入、断线撤权，以及桌面亮色/手机暗色渲染。
- `cargo test -p ponynotes-weknora --lib` 覆盖身份映射、当前可见资料过滤、完整历史分页及原同步回归。
- Linux Rust 1.86 容器运行 `cargo check -p appflowy-cloud --lib`；Windows 本机 Cloud 构建缺少 OpenSSL/MSVC 依赖。
- 尚未发布服务、运行数据库迁移或使用真实商业模型完成端到端验收。上线验收应实际创建和修改笔记、拒绝删除、停止待审批操作、切换账号并恢复历史，确认只由对应用户的 sub2api 账户结算。
