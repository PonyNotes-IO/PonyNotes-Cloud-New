# 47.94.42.183 部署

服务器目录：`/opt/ponynotes`。Cloud、Worker、Java 后端和依赖均由 Docker Compose 管理，数据保存在命名卷，仅 Nginx 的 80 端口对外发布。

## 独立部署 WeKnora

2026-11-06 已在测试服部署 `v0.9.0`，五个核心服务运行正常；已验证管理员登录、系统管理员权限、飞书 Wiki/云盘连接器、浏览器知识库页面，以及容器重建后账号和配置保留。网页现已开放公网访问：[http://47.94.42.183:18080](http://47.94.42.183:18080)，无需 SSH 隧道；外部直连网页和认证配置接口均返回 200，小马笔记原有三个健康检查接口也均返回 200。本地向量模型已配置并验收，问答模型由使用者另行配置，飞书应用尚未配置。

在 `nac` 根目录执行：

```powershell
# 校验锁定的上游文件、Compose、回环端口和密钥保留行为，无需连接服务器
python PonyNotes-Cloud-New/deploy/47.94.42.183/deploy-weknora.py --self-test

# 只检查 SSH、Docker、内存和磁盘
python PonyNotes-Cloud-New/deploy/47.94.42.183/deploy-weknora.py --check

# 部署并检查页面、管理员登录和飞书连接器
python PonyNotes-Cloud-New/deploy/47.94.42.183/deploy-weknora.py

# 可选：打开本机访问隧道，保持此终端运行
python PonyNotes-Cloud-New/deploy/47.94.42.183/deploy-weknora.py --tunnel
```

当前测试服 `.env` 中的 `FRONTEND_PORT=0.0.0.0:18080`，UFW 已放行 `18080/tcp`。网页通过该端口代理知识库 API，后台服务仍绑定 `127.0.0.1:18081`。后续部署保留已有 `.env`，会保留此公网设置；新建实例默认使用回环监听。修改前的配置备份为服务器 `.env.before-public-web-20261106`。

也可以建立隧道后访问 `http://127.0.0.1:18080`。SSH 校验复用本部署目录已记录的测试服务器公钥；可使用 `--key 私钥路径`，部署也支持原有 `PONYNOTES_SSH_PASSWORD` 环境变量，未配置时交互输入密码。

WeKnora 部署独立存放在 `/opt/ponynotes-weknora`，Compose 项目为 `ponynotes-weknora`，不修改 `/opt/ponynotes` 的业务部署。采用官方 Compose 的 frontend、app、postgres、redis、docreader 五个核心服务和项目独立持久化卷；文件存储为 local，不启动额外向量库、沙箱、Langfuse 等可选组件。初始文档处理并发为 2。

锁定上游 `v0.9.0`，源码提交为 `f17ee8a8da901bd658a4cb25f40b5fd41adbfdd7`。注意 Docker Hub 实际发布标签包含 `v`，必须使用 `WEKNORA_VERSION=v0.9.0`。脚本验证官方 Compose 和配置文件的 SHA-256，首次拉取后把实际镜像 digest 写入 `images.lock.json`；后续启动和重新部署使用该锁定文件。

首次部署自动生成独立数据库密码、Redis 密码、JWT 和加密密钥，保存于服务器 `.env`（权限 600），重试保留原值。管理员账号为 `admin@weknora.ponynotes.test`，随机密码保存在服务器 `/opt/ponynotes-weknora/admin-credentials.json`（权限 600）；脚本在本机回环接口完成注册和管理员初始化后关闭公开注册。服务器 SSH 密码不会写入这些文件。

部署成功不代表模型问答或飞书同步已配置。模型端点、Embedding 和飞书企业应用需要在 WeKnora 页面后续配置；部署检查只验证服务、页面、登录与连接器存在。

日常维护在服务器执行：

```sh
cd /opt/ponynotes-weknora
docker compose -p ponynotes-weknora -f compose.yaml -f compose.local.json -f images.lock.json ps
docker compose -p ponynotes-weknora -f compose.yaml -f compose.local.json -f images.lock.json logs --tail 80 app
docker compose -p ponynotes-weknora -f compose.yaml -f compose.local.json -f images.lock.json up -d --no-build --wait
```

停止实例使用同样 Compose 参数加 `stop`；恢复使用 `up -d --no-build --wait`。备份 WeKnora 数据库、文件存储卷、`.env` 和镜像锁定文件；不要删除数据卷或重新生成加密密钥。完整验收状态见服务器 `deployment.json`。

## 原生 Ollama

2026-11-06 已在测试服原生安装 Ollama `0.40.15`，由 `ollama.service` 管理，已启动并启用开机自启。服务器为 4 核 CPU、约 8GB 内存，无 GPU；安装官方 Linux amd64 包中的程序和 CPU 运行库，模型目录为 `/var/lib/ollama/models`，以独立 `ollama` 用户运行。已安装并在 WeKnora 配置 `qwen3-embedding:0.6b` 向量模型；其他语言模型由使用者另行配置。

| 用途 | 地址 |
| --- | --- |
| 服务器本机 API | `http://127.0.0.1:11434` |
| WeKnora 模型服务地址 | `http://172.21.0.1:11434` |

向量模型为官方 Q8_0 版本，约 639MB，模型 digest 为 `ac6da0dfba84a81fdbfbaf330198c33cd77c4cdfc53e8bc50eb581914a15621d`。WeKnora 管理员租户及三个现有同步用户租户分别创建了 `Embedding` 类型、`local` 来源的模型记录，维度为 1024，`truncate_prompt_tokens=2048`；三个同步知识库均已绑定各自租户中的模型记录。知识库现有分块、摘要、问答和其他模型配置保留。

WeKnora 本地模型运行时使用实例级 Ollama 连接。服务器 `.env` 已设置 `OLLAMA_BASE_URL=http://172.21.0.1:11434` 并重建 app 容器生效；修改前的私密备份为 `.env.before-native-ollama-20261106-134957`。新用户或新租户的模型记录需要单独配置，不会自动从管理员租户复制。

已验证版本接口、模型列表、服务重启和 WeKnora 容器网络到原生 Ollama 的访问。UFW 仅为 WeKnora 网桥 `br-43ce50f4bef1` 的 `172.21.0.0/16` 来源增加访问宿主机 `172.21.0.1:11434` 的规则，公网连接测试超时。重建 WeKnora 网络导致网桥或网段改变时，需要同步调整该规则和模型服务地址。

服务配置源文件为本目录 `ollama.service`，服务器实际配置位于 `/etc/systemd/system/ollama.service`；初始限制为 2 核 CPU、3GiB 内存、同时加载 1 个模型、并发 1 个请求，空闲 5 分钟卸载模型。调整后运行 `systemctl daemon-reload && systemctl restart ollama`。当前服务器适合小型 CPU 模型，更大的模型需要评估和调整资源限制。

日常维护在服务器执行：

```sh
systemctl status ollama --no-pager
systemctl restart ollama
journalctl -u ollama -n 50 --no-pager
ollama list
curl -fsS http://127.0.0.1:11434/api/version
```

本次通过本机下载官方 `v0.40.15` 的 `ollama-linux-amd64.tar.zst` 并校验 SHA-256 `628c018404a4206493efd38e46251c154ede5141b8b8dc85c659b3b027c47379`，再传输所需的原生 CPU 文件，逐项验证安装后的文件哈希和符号链接。安装包、来源与文件校验清单、验收记录保存在服务器 `/opt/ollama/`；备份模型时保留 `/var/lib/ollama/`。

向量模型验收已通过：三条中文输入均输出 1024 维有限值向量，相关文本余弦相似度约 0.783，无关文本约 0.201；三个同步知识库共 27 条笔记均为 `completed`。禁用关键词召回后，每个知识库均返回实际向量命中，跨租户知识库请求被拒绝；索引完成、模型已加载时，本轮三个样本查询耗时约 0.10～0.19 秒，Ollama 内存峰值约 1.43GiB。这些是当前少量数据的实测结果，不代表大规模或并发导入性能。完整配置与验收记录见 `/opt/ollama/embedding-deployment.json`、`embedding-validation.json`；知识库修改前的私密备份为 `/opt/ollama/weknora-embedding-before.json`。原有 Cloud、GoTrue、Java 和 WeKnora 网页健康检查均为 200。

## Cloud / Worker 原生知识库

知识库业务统一在 Rust 中：Cloud 的 `/api/knowledge/` 提供列表、创建、私有／共享、收藏、文件、数据源、Wiki、检索和网页导入；Worker 每轮结束五分钟后同步一次。WeKnora 继续负责网页抓取、SSRF 校验、文档解析和检索。旧 Python 业务脚本和安装入口已移除；部署、迁移和冒烟脚本仍是运维工具。

每个工作区只有一个默认库，自建库独立保留。共用服务端租户 Key，Key 不下发客户端。Cloud 校验现有登录 JWT、GoTrue 会话、工作区角色和私有库所有者；检索、文件和 Wiki 在请求时重新过滤页面及个人随手记权限。Owner／Member 可以创建和导入，Guest 只能读取授权资料；默认库由当前空间主人管理，自建库由创建者管理。收藏属于当前用户。

同步覆盖云端文档、数据库结构／记录／记录正文和随手记，保留附件引用；AI 会话不在同步范围。Worker 使用普通成员的短期令牌逐个读取，排除 Guest，任一成员扫描失败均不清理旧索引。全部扫描和写入成功后只清理 `pn:` 受管来源，网页、手动资料和连接器资料保留。数据库来源按可见视图保留标识，随手记写入所属用户；多个 Worker 用 PostgreSQL advisory lock 防止重复运行。

`af_weknora_sync_state.state` 保存共享库映射、服务端凭据和向量代理，`settings` 保存自建库、可见性、收藏和待完成创建。修改设置使用数据库行锁，Worker 只更新自己的状态字段。稳定 UUID 与先保存后创建的流程支持远端响应丢失后的重试。`state.users` 保留已部署 Agent 的独立租户及会话索引，沿用 Rust 同步；公共知识库不使用这些用户索引。Agent 同步与公共库同步共用锁并分开保存报告，保留已有会话和文档 ID。新迁移不修改已应用 SQL 文件。

网页导入先通过 `web_import_options` 读取当前库参数，再用 `import_url` 提交 URL、拆分策略、分块大小与重叠。其余拆分配置保留，由 WeKnora 抓取 URL；重复 URL 返回 409。协议与 `feat/mcp-agent-access` 客户端兼容。

向量计量通过 Docker 内部的 `POST /internal/knowledge/{workspace_id}/v1/embeddings` 完成，不再监听 Python 的 18082／18083。每个空间的随机令牌限制模型与空间；仅允许服务器配置的本地 Ollama。推理前查询有效空间的当前主人，按 `/api/embed` 返回的真实 `prompt_eval_count` 调用现有 Rust `record_usage(AiTokens)`；非法或缺失 usage 不估算，异常向量也会记录已经产生的真实用量。问答继续沿用原有 Rust 主人计费与聊天额度。此迁移保留已有计量范围：小马问答与本地向量化；额外的远端 Embedding、OCR、摘要及 Wiki 生成模型不在该计量范围。

Cloud／Worker 配置（默认关闭）：

```dotenv
PONYNOTES_WEKNORA_ENABLED=true
PONYNOTES_WEKNORA_URL=http://ponynotes-weknora-api:8080/api/v1/
PONYNOTES_WEKNORA_CLOUD_URL=http://nginx
PONYNOTES_WEKNORA_ADMIN_FILE=/run/weknora-admin.json
PONYNOTES_WEKNORA_EMBEDDING_URL=http://appflowy_cloud:8000/internal/knowledge
PONYNOTES_WEKNORA_OLLAMA_URL=http://云端Docker网关:11434
```

仅 Worker 只读挂载管理员文件。Cloud／Worker／WeKnora app 共用独立 `ponynotes-knowledge` 网络，避免两套数据库同名 DNS 冲突。迁移器保留 WeKnora SSRF 白名单并追加精确的 `appflowy_cloud` 回调地址，Nginx 显式拒绝公网 `/internal/knowledge/`，Ollama 仅向 Cloud 所在 Docker 网桥开放。

首次迁移：先用 `deploy-test.py --cloud-only` 构建并更新 Cloud／Worker，工具包同时复制迁移与验收脚本。未导入映射的 Worker 会报告等待迁移，避免提前创建冲突索引。然后在测试服执行：

```sh
cd /opt/ponynotes
python3 migrate-weknora-backend.py --image ponynotes/cloud:本次镜像标签
python3 smoke-weknora-backend.py
```

迁移会短暂暂停 Cloud／Worker 和旧桥接，私密备份配置、SQL 状态、`sync/state.json`、`library/settings.json` 及向量模型配置。现有默认／自建库（包括「神」）、文档、权限、收藏、模型 ID 与维度保留；只修改已有向量模型的回调地址。首轮 Rust 同步成功后切换 Nginx `/api/knowledge/` 到 Cloud 并禁用旧 systemd 服务。失败时恢复配置、模型回调及原服务；保留 SQL 中的待完成 ID／令牌供重试恢复。成功标记后再次运行不会用旧文件覆盖 Rust 状态。备份位于 `/opt/ponynotes/backups/weknora-native-时间/`，含凭据，应随数据库作私密备份。

本地检查：

```powershell
cargo test -p ponynotes-weknora
# 指向独立的临时 PostgreSQL 数据库，名称必须为 weknora_native_test；会清空该库测试表。
$env:TEST_DATABASE_URL='postgres://postgres:本机测试密码@127.0.0.1:55439/weknora_native_test'
cargo test -p ponynotes-weknora -- --include-ignored --test-threads=1
python deploy/47.94.42.183/test-migrate-weknora-backend.py
python deploy/47.94.42.183/test-deploy-test.py
```

日常维护：

```sh
docker compose exec -T appflowy_worker appflowy_worker --weknora-sync-once
docker compose exec -T postgres psql -U postgres -d ponynotes -c 'SELECT last_run,updated_at FROM af_weknora_sync_state;'
docker compose logs --tail 50 appflowy_worker
```

本次迁移源码需随新版镜像上线；仓库修改不代表服务器已更新。

## 测试服聊天模型

2026-11-06 按用户授权，通过 SFTP 只读查找正式服 `101.201.244.231` 的 `/root/docker-compose/.env` 和 `docker-compose-dev.yml`，确认模型配置及其向 Cloud 容器的传入关系；正式服未上传文件、执行 shell 命令或进行服务操作。仅提取模型相关的 11 个 `AI_CHAT_*` 字段，通过 SSH 写入测试服 `47.94.42.183` 的 `/opt/ponynotes/config/cloud.env`，不复制正式服其他环境配置。

| 用途 | 模型 | 服务地址 |
| --- | --- | --- |
| DeepSeek（默认） | `deepseek-v4-1-flash-261017` | `https://ark.cn-beijing.volces.com/api/v3` |
| 通义千问 | `qwen3-vl-plus` | `https://dashscope.aliyuncs.com/compatible-mode/v1` |
| 通义联网搜索 | `qwen-plus` | 同上 |
| 豆包 | `doubao-seed-2-1-pro-260628` | `https://ark.cn-beijing.volces.com/api/v3` |

2026-11-10 按用户要求，仅将测试服 DeepSeek 从 `deepseek-v4-pro-260425` 切换为 DeepSeek V4.1 Flash 最新 1017 版 `deepseek-v4-1-flash-261017`（[火山方舟模型说明](https://www.volcengine.com/docs/82379/2687970)）。复用现有服务地址和 API Key，上游默认模式及显式启用深度思考的 SSE 请求均返回 HTTP 200、正确答案与目标模型 ID。`AI_CHAT_DEEPSEEK_REASONER_MODEL` 为空，深度思考沿用该新模型。配置修改前备份为 `/opt/ponynotes/config/cloud.env.before-deepseek-20261110T021718Z`（权限 `0600`）。仅更新 `AI_CHAT_DEEPSEEK_MODEL` 并重建 Cloud、Worker，已核对两个容器的全部 Compose 环境变量及其他容器 ID；公网 `/health` 和 `/api/ai/chat/models` 均返回 200。本轮未验证客户端实际对话；正式服未修改。

DeepSeek 联网 Bot 字段沿用来源文件的空值，按 Cloud 现有逻辑回退至通义联网模型。配置中的行尾注释已移除，API Key 不写入本仓库、客户端或验收输出。目标配置权限为 `0600`；修改前的私密备份为 `/opt/ponynotes/config/cloud.env.before-ai-20261106T073850Z`。

只重新创建测试服的 `appflowy_cloud` 与 `appflowy_worker`，沿用既有镜像和 Compose 文件；其余测试容器 ID 保持不变。容器内 11 个字段均与提取的来源配置相符。三个对话模型分别对最小问题返回 HTTP 200 和正确答案；公网 `/api/ai/chat/models` 返回三个模型，`/health` 返回 200。此轮验证不包含联网搜索、图片或深度思考模式。

后续修改此文件后，在测试服 `/opt/ponynotes` 执行以下命令使环境变量生效；单独 `restart` 不会更新容器环境：

```sh
docker compose config --quiet
docker compose up -d --no-deps --no-build --pull never --wait appflowy_cloud appflowy_worker
```

如需回退，先将上述备份恢复为测试服的 `config/cloud.env`，再执行同样的容器更新命令。现有部署脚本保留服务器 `config/`，模型密钥无需随代码上传。

## 一键更新三个后端

在本机 `nac` 根目录运行（PowerShell、终端均可）：

```powershell
python PonyNotes-Cloud-New/deploy/47.94.42.183/deploy-test.py
```

仅更新 Cloud 和同镜像的 Worker 时使用 `--cloud-only`。此模式只打包 Cloud 当前工作区，保留服务器现有 GoTrue/Java 镜像配置，不构建或重启这两个服务；仍执行数据库备份、Cloud 迁移和现有冒烟验证。知识库首次迁移随后执行上文 `migrate-weknora-backend.py`；完成后随 Cloud／Worker 正常更新，无需安装桥接。

脚本随 Cloud 仓库保存，三个项目目录应位于同一个 `nac` 工作区。当前本机原命令 `python deploy/47.94.42.183/deploy-test.py` 仍通过入口文件转发，其他电脑请使用上面的仓库内路径。

要求 Python 3.11+ 和 Paramiko，本机已安装；其他电脑首次使用可执行 `python -m pip install paramiko`。默认只提示一次测试服 root 的 SSH 密码，不将密码写入文件。已设置 `PONYNOTES_SSH_PASSWORD` 环境变量时会使用该值；也可以传 `--key C:\path\to\private_key` 使用 SSH 私钥。

脚本固定连接测试服 `47.94.42.183`，校验已记录的 SSH 主机公钥。它打包本地 `PonyNotes-gotrue`、`PonyNotes-Cloud-New`、`PonyNotes-Web-Desktop` 的源码（包含未提交修改），在服务器构建 GoTrue、Cloud/Worker 镜像和 Web-Desktop 的 Java 后端 JAR。Vue/Nuxt 网页不属于此脚本的部署范围。无需在本机安装 Rust、Go、Java 或 Maven；Java 构建使用 `maven:3.9.9-eclipse-temurin-17` 容器，每次先清理旧编译产物，依赖保存在 `ponynotes_maven_cache` 卷。

当前脚本用于更新已初始化的测试服务器，不负责新服务器初始化。它保留服务器 `.env`、`config/`、Nginx、基础服务和数据卷，包括现有模拟验证码配置。每次构建使用独立镜像标签，通过受管的 `compose.override.yaml` 选择版本；不会用本地 Compose 覆盖服务器的配置。Rust/Go 编译和依赖缓存继续复用。服务器存在其他工具创建的 `compose.override.yaml` 时，脚本会停止以免覆盖。

所有构建成功后，脚本备份 PostgreSQL，运行现有 Cloud 迁移程序，再依次更新认证、Cloud、Worker 和 Java 服务。GoTrue 自身迁移随启动执行；Java 仓库中的 MySQL SQL 文件不会自动执行。随后检查容器健康状态，并运行真实认证/API 冒烟检查；已配置专用测试 OTP 时也运行验证码检查。正常更新不会清库或重导管理员账号。

源码快照、执行日志位于 `/opt/ponynotes/releases/test-时间-随机ID/`；更新前的配置、旧 JAR、数据库备份位于 `/opt/ponynotes/backups/相同ID/`，旧镜像保留。失败时返回非零退出码并显示日志位置，数据库不会自动回滚。构建阶段不会停止运行中的服务。部署记录和源码提交信息更新到服务器 `deployment.json`。

其他用法：

```powershell
# 只检查 SSH、服务器工具及现有部署配置
python PonyNotes-Cloud-New/deploy/47.94.42.183/deploy-test.py --check

# 只生成源码包，不连接服务器（不含 .env、构建产物和网页前端）
python PonyNotes-Cloud-New/deploy/47.94.42.183/deploy-test.py --bundle-only test-backends.tar.gz

# 检查打包规则
python PonyNotes-Cloud-New/deploy/47.94.42.183/test-deploy-test.py
```

| 服务 | 地址 |
| --- | --- |
| Cloud 健康检查 | http://47.94.42.183/health |
| Cloud API | http://47.94.42.183/api/server |
| Cloud WebSocket | ws://47.94.42.183/ws/v2/{workspace_uuid}（需认证参数） |
| GoTrue | http://47.94.42.183/gotrue/health |
| 客户端密码状态 | http://47.94.42.183/gotrue/user/password/status?email=用户邮箱 |
| Java 后端 | http://47.94.42.183/prod-api/api/health |

管理员账号和随机密码位于服务器 `/opt/ponynotes/config/admin-credentials.txt`。运行密钥在 `.env`、`config/cloud.env` 和 `config/gotrue.env`，不要提交到 Git。

```sh
cd /opt/ponynotes
docker compose ps
docker compose logs --tail 100 appflowy_cloud web_backend
docker compose up -d --pull never
python3 smoke.py
python3 test-otp-smoke.py
```

Cloud 按本次上传的工作区源码快照和 Cargo.lock 编译；版本、迁移数量及校验值见服务器 `deployment.json`。构建机使用 Ubuntu 26.04 的 Rust 1.93.1、protoc、libprotobuf-dev、rustfmt。`build-cloud.sh` 同时构建 Cloud 和 Worker，再打包 Docker 镜像。Java 使用工作区构建的 `ruoyi-admin.jar`，通过 `PONYNOTES_CLOUD_URL` 连接容器内 Cloud。

MinIO 使用官方 `RELEASE.2025-09-07T16-13-09Z` 源码构建，原下载服务已返回不可用；`build-minio.sh` 使用 Go 工具链和固定版本源码压缩包，构建记录中的实际工具链为 Go 1.26.8。源码与构建日志保留在服务器，重新构建分别执行 `bash build-cloud.sh`、`bash build-minio.sh`。

GoTrue 基于 `PonyNotes-gotrue` 提交 `73d0a1ebfb0bf20bfa90b72389e76ad4d2807ff5`，2026-10-29 加入按测试邮箱配置 OTP 的本地修改。自动部署镜像标签为 `ponynotes/gotrue:test-时间-随机ID`，当前实际版本见服务器 `deployment.json`。手机号复用 GoTrue 已有测试 OTP。服务器保留 `gotrue-source/` 和源码压缩包，通过 `bash build-gotrue.sh` 重建；仅编译本机架构，复用已有运行时镜像。升级前数据库备份在 `backups/postgres-before-custom-gotrue-20261028.dump`，模拟验证码修改前的配置、Compose 和修改涉及的原源码位于 `backups/before-test-otp-20261029-091315/`。定制迁移已自动执行，认证迁移共 61 个。

新数据库由 `postgres-init.sql` 和 `config/mysql-init.sql` 初始化，已有卷启动时不会重导数据。GoTrue 管理员使用容器内 `auth admin createuser --admin --confirm EMAIL PASSWORD` 创建，密码与 `.env` 中的 `ADMIN_PASSWORD` 一致；Java 管理员使用另一组随机密码。

首次创建空数据库时，先启动 `postgres mysql redis minio gotrue`，等待 GoTrue 初始化认证表并创建管理员，再执行下面三步，最后启动其余服务。已有数据库无需重复初始化。

```sh
docker compose exec -T postgres psql -U postgres -d ponynotes -v ON_ERROR_STOP=1 < auth-compat.sql
python3 migrate-cloud.py
docker compose exec -T postgres psql -U postgres -d ponynotes -v ON_ERROR_STOP=1 < seed-plans.sql
```

原仓库混用 8 位和 14 位迁移编号，`migrate-cloud.py` 按日期执行并记录原 SQLx 校验值，重跑会验证已执行迁移。`auth-compat.sql` 补充 Cloud 所需的密码状态字段及触发器；`seed-plans.sql` 补充配额代码依赖的三个计划。不要运行 `docker compose down -v`，该命令会删除数据库及文件卷。

测试服运行 GoTrue、Cloud、Cloud Worker 和 Web-Desktop Java 后端。当前入口使用 IP 和 HTTP；域名、HTTPS 和第三方平台回调需按正式域名配置。AI API Key 未提供，独立 AI 服务与向量索引未启用。

定制认证 API 已部署：Flutter 客户端实际使用 `GET /gotrue/user/password/status`，支持 `email` 或 `phone` 参数。GoTrue 复用现有 SMTP 配置，邮箱注册要求验证邮箱。2026-10-29 排查确认真实邮件验证码投递失败：`/otp` 返回 500，QQ SMTP 返回 `535 Login fail`；到 `smtp.qq.com:465` 的连接正常，需检查 SMTP 开关、账号状态及授权码。当前未找到短信、微信或抖音的正式凭据，真实短信服务尚未配置，第三方登录需在 `config/gotrue.env` 补充配置并按正式域名设置回调。正式服 `root@101.201.244.231` 的 SSH 认证未通过，暂未能从正式服提取配置。

2026-10-29 已为测试服启用专用账号的模拟验证码。开发使用 `debug@ponynotes.test` 或 `19900000001`，先获取验证码，再输入 `123456`；不会实际发送邮件、短信，会签发真实认证会话。仅配置列表内的账号生效，其他账号仍需真实服务商。邮箱和手机号列表位于 `config/gotrue.env` 的 `GOTRUE_MAILER_TEST_OTP`、`GOTRUE_SMS_TEST_OTP`，默认不启用该模式。`otp-smoke@ponynotes.test` 和 `19900000002` 是可删除的自动检查账号；`test-otp-smoke.py` 覆盖注册、错误码拒绝、重复登录、刷新及 Cloud 接口并清理账号。已通过 Go 邮箱测试 OTP 配置检查；未向正式服部署修改。

验证已通过：Cloud 认证与刷新令牌、工作区、文件上传下载及清理、WebSocket 握手、Java 验证码登录、管理员信息与菜单、跨库读取订阅计划、未认证访问拦截，以及定制 GoTrue 的密码状态、参数校验和密码修改。认证测试通过管理员 API 创建临时已确认用户，验证新旧密码后删除。Docker 已启用开机启动，所有容器设置自动重启和持久化卷；移除临时构建代理并重启 Docker 后复测通过。验证未发起支付、邮件投递或发送短信。
