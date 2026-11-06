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

## Cloud / Worker 原生 WeKnora 集成

笔记同步和检索运行在项目原有 Rust 后端中：Cloud 提供 `POST /api/knowledge/search`，Worker 启动后运行一次同步，每轮结束五分钟后再次运行。共用代码位于 `libs/weknora`，复用现有 PostgreSQL、用户令牌、工作区权限和 Cloud 文档 API，不增加独立同步/检索进程或端口。WeKnora 继续负责文档处理、Embedding 和混合检索。

范围包含已同步到服务器的文档正文、数据库结构与记录正文、个人随手记。按 Cloud 用户隔离 WeKnora 租户，每个工作区对应独立知识库；过滤回收站和其他用户的私有页面。附件保留引用，不下载或 OCR；AI 聊天会话不属于笔记同步范围。新增、修改、删除在下一轮成功扫描后同步，来源读取或写入失败时不会清理旧副本。使用 PostgreSQL advisory lock 防止多个 Worker 同时同步。

迁移保留原有用户、租户、知识库、文档 ID 和模型配置。原来的私密 `sync/state.json` 一次性导入 `af_weknora_sync_state.state`，后续由 Worker 写入数据库；该状态含租户凭据，随 Cloud 数据库私密备份，不通过接口返回或记录日志。检索只加载当前登录用户的映射，并实时复核 GoTrue 登录身份、工作区成员资格和页面可见性；客户端传入的用户、租户和知识库 ID 不参与选择。

配置由 Cloud/Worker 共用的 `config/cloud.env` 提供，默认关闭，迁移前已有部署不受影响：

```dotenv
PONYNOTES_WEKNORA_ENABLED=true
PONYNOTES_WEKNORA_URL=http://ponynotes-weknora-api:8080/api/v1/
PONYNOTES_WEKNORA_CLOUD_URL=http://nginx
PONYNOTES_WEKNORA_ADMIN_FILE=/run/weknora-admin.json
```

Cloud、Worker 和 WeKnora app 通过独立的 `ponynotes-knowledge` 内部网络连接，避免两套 PostgreSQL/Redis 同名 DNS 冲突；只有 Worker 只读挂载管理员凭据文件，创建新用户映射时使用。现有账号继续使用各自的租户 Key。新租户的 Embedding 模型仍需在 WeKnora 单独配置，未配置时先导入草稿；配置后下一轮自动发布。

首次从旧 Python 桥接迁移，在测试服构建包含本次代码的 Cloud/Worker 镜像后执行：

```sh
python3 migrate-weknora-backend.py --image ponynotes/cloud:本次镜像标签
```

脚本仅针对测试服 `/opt/ponynotes`：备份 Compose、Nginx、环境配置和原始同步映射，停止旧同步任务，应用新增表迁移并导入映射，更新 Cloud/Worker，移除 Nginx 到旧 `18082` 的转发。原生同步首轮成功后停用旧同步 timer/service 和检索 service；失败自动恢复旧配置和服务。备份位于 `/opt/ponynotes/backups/weknora-native-时间/`。部署脚本中的 Python 仅用于运维，日常同步和检索不依赖 Python。新网络和挂载写入保留的基础 Compose，后续现有 `deploy-test.py` 更新仍可沿用。

日常维护和验证：

```sh
cd /opt/ponynotes
# 手动执行一轮；与后台任务使用同一把数据库锁
# 若提示 already_running，等待当前轮结束后再执行。
docker compose exec -T appflowy_worker appflowy_worker --weknora-sync-once
# 查看不含正文或凭据的同步摘要
docker compose exec -T postgres psql -U postgres -d ponynotes -c 'SELECT last_run,updated_at FROM af_weknora_sync_state;'
docker compose logs --tail 50 appflowy_worker
# 原生集成回归检查
cargo test -p ponynotes-weknora --lib
# 测试服实际身份、租户隔离及新增/修改/删除验证（结束清理临时随手记）
python3 smoke-weknora-backend.py
```

Flutter 的「知识库检索」开关和接口协议保持兼容：开启后先检索当前用户当前工作区的资料，再交给原有聊天模型生成回答。最多返回 5 条去重来源、总计 16000 字符；无资料和待同步状态保持原有语义。对话模型仍使用小马聊天 API 的配置。

2026-11-06 已在测试服完成迁移：镜像 `ponynotes/cloud:weknora-native-20261106-173132-agent`，数据库迁移编号 `20261106020000`，保留服务器已有 Agent 功能。8 项 Rust 回归检查通过，实际验证 3 位用户的检索和权限隔离，以及临时随手记的新增、修改、重复运行和删除；3 个原有知识库、32 个文档 ID 和模型绑定均保留。旧 Python 同步和检索 unit 已停用并禁用开机启动。服务器部署记录位于 `/opt/ponynotes/deployment.json`，本次配置备份位于 `/opt/ponynotes/backups/weknora-native-20261106-181011/`；临时测试数据已清理。

## 测试服聊天模型

2026-11-06 按用户授权，通过 SFTP 只读查找正式服 `101.201.244.231` 的 `/root/docker-compose/.env` 和 `docker-compose-dev.yml`，确认模型配置及其向 Cloud 容器的传入关系；正式服未上传文件、执行 shell 命令或进行服务操作。仅提取模型相关的 11 个 `AI_CHAT_*` 字段，通过 SSH 写入测试服 `47.94.42.183` 的 `/opt/ponynotes/config/cloud.env`，不复制正式服其他环境配置。

| 用途 | 模型 | 服务地址 |
| --- | --- | --- |
| DeepSeek（默认） | `deepseek-v4-pro-260425` | `https://ark.cn-beijing.volces.com/api/v3` |
| 通义千问 | `qwen3-vl-plus` | `https://dashscope.aliyuncs.com/compatible-mode/v1` |
| 通义联网搜索 | `qwen-plus` | 同上 |
| 豆包 | `doubao-seed-2-1-pro-260628` | `https://ark.cn-beijing.volces.com/api/v3` |

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
