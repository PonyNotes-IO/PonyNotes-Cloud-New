# 模型管理到 WeKnora 的业务接口

当前部署为一个 WeKnora 账号、一个租户。小马通过工作区与知识库 ID 的服务端映射管理多个知识库。

模型配置保存到这个既有租户的 WeKnora 模型表；API Key 交由 WeKnora 的加密凭据存储保存。客户端仅经 Cloud 的已登录接口操作，不保存知识库模型密钥，不创建租户。小马服务端校验模型归属与知识库管理权限。

## 类型与使用位置

| 页面分类 | WeKnora 类型 | 使用位置 | 配置字段 |
| --- | --- | --- | --- |
| 对话 | KnowledgeQA | 知识库摘要、问答模型事实 | 地址、密钥、模型名、协议、上下文、输出上限 |
| Embedding | Embedding | 知识库入库向量化与向量检索 | 地址、密钥、模型名、维度、输入 token 上限 |
| ReRank | Rerank | 知识库 hybrid-search 的 rerank 参数 | 地址、密钥、模型名；Top K 在知识库使用设置中填写 |
| 视觉 | VLLM | 知识库 vlm_config 图像解析 | 地址、密钥、模型名、协议、上下文、输出上限 |
| 语音 | ASR | 知识库 asr_config 转写 | 地址、密钥、模型名、厂商所需字段 |

服务当前原生语音类型是 ASR，按语音转写落地。温度是推理请求参数，当前 WeKnora 模型表不支持，不能画一个不会生效的滑块。

## API 与归属

复用 POST `/api/knowledge/{action}`：models、model_providers、model_save、model_delete、model_settings、model_bind。请求携带 workspace_id；绑定携带 base_id。服务端从映射取得实际知识库 ID，客户端不能选择租户或传入 WeKnora 管理密钥。

工作区所有者创建/编辑该工作区的自定义模型。模型存储在同一个 WeKnora 租户，以服务端描述标记限制小马配置归属。知识库响应的 `is_builtin` 保留 WeKnora 原始值；`can_edit` 仅对当前工作区拥有的非内置模型为 true，共享租户中的手工配置保持只读。API Key、租户管理凭据和自定义请求头不回传。

删除使用 `model_delete`，请求为 `{workspace_id, id}`。仅工作区所有者可删除本工作区自定义模型；内置模型与其他工作区模型拒绝删除。先在共享 advisory lock 下检查 Cloud 中保存的模型绑定，再由 WeKnora 检查知识库、智能体与记忆引用；使用中的模型需先解除绑定。客户端确认后才发送请求，失败保留卡片。2026-11-18 已在测试服补齐删除动作白名单、分发入口和写权限分类，真实临时模型的编辑保存、原ID回读和删除验收通过。发布记录在根工作区 `deploy/model-actions-20261118/README.md`；下文保留之前的模型管理部署记录。

知识库管理者在“知识库使用设置”按库绑定模型。向量/摘要/视觉/语音绑定 PATCH 到真实知识库；ReRank 选择保存到既有知识库 settings，在检索时传到 hybrid-search。厂商清单、类型支持、默认地址、厂商字段和模型目录直接读取当前 WeKnora `/models/providers?model_type=...`，不复制客户端静态厂商表。

已有文档的知识库不切换 Embedding；既有 Embedding 模型的模型名、地址和维度也不能就地改变，需另建模型和新库。不会自动清空或重建向量。绑定与后台同步使用相同数据库 advisory lock；同步保留显式绑定，避免恢复默认代理。

设置页将 `/api/ai/agent/models` 的三个 Agent 内置模型与此处的真实内置模型、本工作区自建模型（`can_edit=true`）合并展示：顶部统一类型筛选，下方按“小马内置模型／自定义模型”分组。服务端共享手工配置从管理列表及数量统计中排除，接口原始记录和已有绑定保留。三个 Agent 模型归入“对话”，卡片展示名称、供应商及类型；所有卡片本体均无点击事件，原详情弹窗已移除。自定义卡片的编辑、删除仅由操作图标触发并遵守权限，中间链条暂为无操作的分享占位。Agent 的稳定选择 ID 不进入知识库 UUID 绑定、编辑或删除请求。原有问 AI 自定义对话配置的保存位置、选择入口与调用方式继续使用。

## 验证

类型参数隔离、跨工作区模型不可见、绑定类型校验、凭据响应隐藏、ReRank 请求透传由 Rust 测试覆盖；客户端接口与表单另测真实请求路径、保存字段与分类筛选。

## 测试服部署与验证（2026-11-17）

已部署至 `47.94.42.183`。Cloud 和 Worker 使用镜像 `ponynotes/cloud:model-management-20261117`，从正在运行的 `test-20261113-120950-0eae782e` 源码建立候选，只覆盖本次模型管理相关文件。服务端 Linux 的 release 构建成功，WeKnora 集成库30项测试通过（已有数据库测试1项忽略）。没有新增 migration。

复用当前单一 WeKnora 账号/租户。为 Cloud 增加与 Worker 相同的 `/run/weknora-admin.json` 只读挂载，以读取既有管理员凭据；服务环境变量保持原值。没有重建 WeKnora、数据库、GoTrue、Nginx 或其他业务容器。

真实账号接口已验证：模型列表不回传密钥，五种类型的厂商目录可用，匿名和跨工作区请求被拒绝，ReRank 拒绝向量字段，自定义模型新建/编辑成功，临时知识库的模型绑定和回读成功，原知识库的向量模型保持不变且有文档时锁定。测试模型和测试知识库已删除，未调用临时模型做推理。ReRank 的实际检索请求参数由集成库的HTTP fixture覆盖。

公网 `/health`、`/api/server`、`/api/ai/chat/models` 均为200；匿名调用 `/api/knowledge/models` 为401。Cloud健康、Worker运行正常，最近一次知识库后台同步错误数为0。

- 发布目录：`/opt/ponynotes/releases/test-model-management-20261117/`
- 构建日志：发布目录 `build.log`
- 发布及验收日志：发布目录 `deploy.log`；本机副本在工作区 `outputs/model-management-20261117/deploy-test-server.log`
- 数据库与原运行配置备份：`/opt/ponynotes/backups/model-management-20261117/`，包含Cloud和WeKnora的PostgreSQL备份。
- 回滚：在测试服执行 `bash /opt/ponynotes/releases/test-model-management-20261117/rollback.sh`，恢复原Cloud/Worker镜像选择及Cloud凭据挂载状态。无需回滚数据库结构。

客户端相关回归37项通过，最终模型页面5项交互及7个实际Flutter渲染状态通过；当前源码包含最新页面，但本次未重新打包客户端。本机Windows的全量Cloud构建缺OpenSSL，测试服Linux构建已成功；严格Clippy另有既有wiki参数数目和sync HashMap写法两项问题，本次没有修改它们。

视觉与交互记录位于客户端根目录 `design-qa.md`，截图及本机测试日志位于工作区 `outputs/model-management-20261117/`。
