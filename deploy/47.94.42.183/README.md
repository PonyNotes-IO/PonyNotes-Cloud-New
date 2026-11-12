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

## Python 桥接实例的同步、知识库与计费

以下内容适用于仍使用 Python 桥接的测试服实例。上文 Cloud / Worker 原生集成及其迁移工具继续保留；两个运行方式分别使用文件和数据库中的同步状态。已迁移到原生集成的实例应沿用上文部署流程，避免同时启动两套同步。分类、独立知识库创建和向量 token 计量目前由本节桥接接口提供，原生 Rust 路由仍提供已有检索接口。

### 云端笔记同步到 WeKnora

在已部署的实例上安装/更新同步程序并启动定时任务：

```powershell
python PonyNotes-Cloud-New/deploy/47.94.42.183/deploy-weknora.py --sync-notes
python PonyNotes-Cloud-New/deploy/47.94.42.183/test-sync-weknora.py
```

当前仓库已改为**一个工作空间一个知识库**，同一空间的成员检索相同的知识库，不再按成员分别创建副本。同步程序仅使用 Python 标准库，复用现有 Cloud 与 WeKnora API；主人计费需要先升级 Cloud，侧栏入口需要重建客户端。首次全量导入，随后每轮结束五分钟后再次运行；一次扫描超过五分钟时再改事件队列。范围为文档正文、数据库结构、数据库记录及记录正文、个人随手记，过滤回收站。AI 聊天会话不在同步范围内；图片、文件等保留引用和描述，不下载附件或执行 OCR；富文本按云端 API 提供的纯文本导出。

新知识库由既有的服务端管理员租户管理，知识库 UUID 仅由不可变的工作空间 UUID 派生；成员退出、空间改名或转移所有者均不会更换知识库。`/opt/ponynotes-weknora/sync/state.json` 中的 `workspaces[workspace_id]` 保存唯一知识库映射及同步摘要，`service` 保存租户及同步 Key。目录权限 700、文件 600，密钥不下发客户端，也不向成员提供服务端管理员的原生网页登录凭据。此文件必须私密备份，不得提交 Git、放入网页目录或发送给其他用户。

程序从 Cloud 数据库读取未删除工作空间及有效普通成员列表，在服务器内为各成员生成十分钟有效的普通 `authenticated` 令牌，通过 Cloud 原有权限校验读取其可见内容，再按来源去重汇总到空间知识库。签名密钥不写入同步状态、不传给 WeKnora；同步 Key 仅授予 `retrieve`、`ingest`、`manage_kbs`、`manage_datasources` 四项能力。最后一项用于桥接读取数据源列表，WeKnora 将此读取操作纳入连接器管理权限；桥接只开放列表并过滤凭据和配置，Key 不下发客户端。已有受管 Key 原地更新能力，无需更换密钥。访客不是同步主体；任一成员的读取失败都会使该空间本轮失败，不能视为空空间。

来源以稳定 `channel` 标识，创建成功但响应丢失时可恢复，不重复创建。同一来源在成员之间去重；数据库的不同来源视图保留各自标识，避免私有视图遮蔽其他成员可访问的视图。标题、正文、目录变化覆盖副本，小马原文为真源。完整读取全部成员的来源并成功更新后，才清理已不存在的受管笔记；其他渠道导入的资料不参与该清理。成员退出或账号删除/封禁只影响其来源范围，不删除空间知识库；只有空间确实从未删除空间清单消失时才删除该知识库。没有有效成员时保留知识库并报告失败。共享存储不会扩大页面权限：检索时重新检查当前成员资格和页面可见性，随手记按所属用户过滤。内容更新及索引删除仍在下一次成功同步后生效。

新建知识库优先选择管理员租户的默认本地 Embedding 模型；没有默认但只有一个时使用该模型。每个空间创建独立的服务端模型绑定，经私有代理调用相同 Ollama 模型并记录实际 token，维度与截断参数保持原值。没有模型或多个模型未指定默认时导入为 `draft`，下轮有可用模型后再发布。已有非空空间知识库若尚未接入计量代理，会停止同步并保持未就绪，防止直接更换模型绑定破坏旧索引；需先备份并安排重建索引，程序不会自行删除资料。条目的 `parse_status=completed` 才表示处理完成，首次同步的 `ready` 仅表示来源已全部提交，`pending`/`failed` 不代表可检索。对话模型需另行配置。

#### 从用户副本迁移到工作空间知识库

执行上面的 `--sync-notes` 即会同时更新同步、检索两个脚本；`--knowledge-search` 是同一操作的兼容别名。安装器先停同步任务，并将旧脚本保留为 `*.py.before-workspace-kbs`，再先启用新版检索权限检查、启动新版同步。首次修改旧状态前自动保存 `sync/state.before-workspace-kbs.json`（权限 600）。

新知识库在服务端租户下重新扫描 Cloud 原文并建立索引，首次完整提交前返回 `awaiting_sync`。旧的 `users` 映射、个人租户及其知识库只作历史备份，不再被当前同步或聊天检索使用，也不会自动删除。直接在旧 WeKnora 个人知识库上传、且不属于 Cloud 原文的资料不会自动复制，需要另行导入目标空间知识库。迁移后的共享知识库仅经小马鉴权入口访问。

需要回退时，停止同步和检索服务，恢复两个 `*.py.before-workspace-kbs` 脚本及 `state.before-workspace-kbs.json`，再启动检索服务和同步定时器；保留新版空间知识库，不删除数据卷。历史备份只在首次迁移时写入，不会在后续重试中覆盖。

运维命令：

```sh
# 仅读取来源，不修改 WeKnora
python3 /opt/ponynotes-weknora/sync-weknora.py --scan-only
# 立即同步 / 查看上次摘要与失败
systemctl start ponynotes-weknora-sync.service
cat /opt/ponynotes-weknora/sync/last-run.json
journalctl -u ponynotes-weknora-sync.service -n 30 --no-pager
systemctl status ponynotes-weknora-sync.timer --no-pager
# 暂停定时同步，保留数据
systemctl disable --now ponynotes-weknora-sync.timer
```

日志只输出工作区 UUID、成员数量、状态和错误类型，不输出笔记正文、密码或令牌。`coverage` 是各成员扫描数量之和，去重后的实际条目数量见 `parts`。`state.json` 缺失时应恢复备份，以保留同步摘要及旧映射。临时网络失败会让服务返回非零并保留错误摘要，下一次定时运行继续重试。

2026-11-11 本地离线回归通过：同一空间两名成员共用一个知识库、新成员无需个人 WeKnora 账号、跨空间隔离、私有页和个人随手记过滤、成员撤权、转移所有者、重复同步、创建响应丢失恢复、成员扫描失败保留数据、旧副本保留及空间删除清理。同日已部署到测试服，工作空间同步完成且错误数为 0。

2026-11-06 已部署并验收：3 位有效云端用户、3 个隔离知识库、14 篇文档、2 个数据库结构、10 条数据库记录，共 26 条草稿。定时器已启用，服务最近一次退出码 0、错误数 0、26 条均无变化。用临时源文档和随手记实际验证了新增、改名、追加正文、修改、删除与重复运行；普通网页登录令牌和租户 API Key 均无法跨用户读取，模拟来源超时不会清理副本。临时数据已清理，原有 Cloud/GoTrue/Java 健康检查均正常。离线回归检查另覆盖目录超过十层、随手记分页、长文拆分、创建响应丢失后的恢复及工作区撤权清理。真实模型索引/问答尚未验证。

### 小马聊天的知识库检索开关

#### 客户端知识库分类与权限

侧栏「共享」下方的「知识库」入口可展开「私有知识库」「共享知识库」「最爱知识库」，各分类可继续展开具体名称。每个空间保留一个自动同步云端笔记的默认库，空间所有者可修改默认库的私有／共享权限。选中私有或共享分类时，右侧加号可输入名称并创建独立空库，成功后刷新列表并打开新库；不会创建或重命名工作空间。新建私有库仅创建者可见，共享库对所属空间成员可见，创建者可修改可见性；离开空间后失去库访问权限。收藏按用户及库分别保存，不授予额外访问权限。库级权限同时用于列表、内容预览和聊天检索；同步笔记的原页面、个人随手记权限继续生效。

复用现有鉴权桥接服务，无新增数据库表。`POST /api/knowledge/bases` 列出可访问库，`create` 创建库，`visibility` 修改可见性，`favorite` 设置收藏，`documents` 列出授权内容，`document` 返回指定条目的文本预览（最多 200000 字符）。库记录新增 `id`；除列表外请求均包含 `workspace_id`，针对独立库另传 `base_id`，省略时仍指向默认库。`create` 传入 `name`（去首尾空格后 1–100 字符）、`visibility` 和 UUID `request_id`，仅空间所有者和普通成员可创建，访客不能创建。重试须沿用同一 `request_id` 及名称／可见性，服务器可恢复远端创建成功但响应丢失的请求。新库复用所属空间的计量 Embedding 模型；尚未配置该模型时创建返回 503。身份与权限由服务端实时读取 Cloud 数据确认，不接受客户端自报所有者。

点击侧栏「知识库」直接打开当前空间的知识库，中间栏保留「文件 / Wiki / 图谱」和库内搜索，右侧预览选中内容。文件列表包含授权的云端同步条目及库内上传、网页、手动条目；同步条目仍检查原页面或随手记权限。`sources` 返回当前库实际绑定的数据源名称、类型和同步状态，不返回连接凭据；`wiki` 分页读取已启用 Wiki 的库，仅返回当前账号可读取全部来源的页面及其可见连线，`wiki_page` 用 `slug` 获取正文并重新鉴权。未启用 Wiki 的库返回空列表。图谱复用客户端现有画布显示这些 Wiki 页面之间的引用。

网页导入需更新客户端和桥接脚本：点击具体知识库的「＋ → 导入网页」，输入 HTTP/HTTPS 地址后进入解析确认页。`web_import_options` 读取当前库的拆分策略、分块大小和重叠；`import_url` 校验空间成员写权限并调用 WeKnora 原生 `knowledge-bases/{id}/knowledge/url`，仅覆盖本次导入的拆分参数，其余处理设置沿用当前库。URL 抓取及 SSRF 防护由 WeKnora 负责，重复地址返回 409，服务凭据不返回客户端。Flutter 弹窗参照上游 `v0.9.0` 的 `frontend/src/views/knowledge/components/KbUploadSourceDropdown.vue`；没有新增解析器或依赖。

设置写入独立的 `/opt/ponynotes-weknora/library/settings.json`（目录 700，文件 600），避免与同步进程覆写 `sync/state.json`。单实例服务用线程锁和原子替换保存。安装器更新 Nginx `/api/knowledge/` 代理，并只为此目录开放 systemd 写权限。执行上文 `--sync-notes` 或 `--knowledge-search` 安装新版桥接，客户端需重新构建；服务端接口已在测试服部署。

独立库记录及创建恢复状态也保存在 `library/settings.json`，不参与默认库的云端镜像同步。侧栏展开和加号创建需要一并更新客户端及桥接脚本；此新增创建接口的本地离线回归覆盖中文名称、私有隔离、共享与收藏、访客拒绝、成员撤权、创建重试恢复及默认同步状态保持。

2026-11-11 修复测试服创建失败：原服务的 `POST /api/knowledge/create` 返回 404，客户端显示为连接失败。已备份并更新 `/opt/ponynotes-weknora/search-weknora.py`，重启桥接服务；通过 Nginx 实测共享／私有库创建、相同请求重试、列表刷新、空库内容、向量模型绑定和权限隔离，两个临时验收库已清理，默认同步及计费状态未改变。记录见服务器 `library/create-verification.json`，更新前备份在 `backups/create-base-20261111-065107/`。已有支持创建的客户端可直接重试；客户端源码也增加了创建接口未更新时的明确提示。

2026-11-11 修复 WeKnora 点击卡片打开设置的问题：原生网页同时要求 `embedding_model_id` 和 `summary_model_id`，自动创建的库此前缺少后者。同步程序现优先选择启用的默认 `KnowledgeQA` 模型；没有默认但仅有一个启用模型时使用该模型，选择有歧义时保留未配置状态。默认空间库和小马新建独立库均携带该绑定，已有默认库只补缺失值，保留用户已有选择；部分更新通过 WeKnora `PATCH` 接口提交。测试服已为四个空间库及「神」库补上已有 `deepseek-flash`，原有 44 篇文档、向量模型、分块和索引配置逐项比对保持原值。浏览器实测点击 15 篇文档的卡片直接进入文档列表，显示 15 行且无设置弹窗。私密备份在 `backups/initialize-models-20261111-162052/`，服务端验收记录为 `library/initialization-verification.json`。

#### 知识库管理员与主人计费

知识库的默认管理员为 Cloud 中当前工作空间的主人。修改私有/共享权限时实时核对主人；转让空间后，新主人取得管理权限。知识库 ID 和已有资料不随转让改变。

沿用现有套餐口径：问答模型接收请求后扣一次 `ai_chat`；纯检索、自动同步的向量化不额外扣问答次数。问答及向量化实际返回的 token 记录为主人的 `ai_tokens`，由现有订阅用量表保存，`GET /api/subscription/current` 的 `usage.ai_tokens_used_this_month` 返回当前计费月总量。问答剩余次数按主人的订阅汇总；主人的多个空间共用其额度，额度不足不会改扣成员账号。

问答接口先验证空间存在且请求者为成员，再确定扣费主人。提供无效空间 UUID 返回 400，无权访问或已删除空间返回 403；均在调用模型和扣费之前拒绝。未提供空间的独立个人聊天保留个人计费。每项操作开始时确定当时的主人，转让只影响之后的操作，历史用量不重写。

问答从 DeepSeek、通义千问和豆包的 SSE `usage` 读取实际 token；兼容拆包和重复累计 usage，不按字符数估算。向量化复用当前本地 Ollama，通过服务内的独立线程池监听私有 `18083`，读取 `/api/embed` 的 `prompt_eval_count`；缺失或非法 usage 会返回不可用，避免伪造统计。每个空间的随机服务密钥保存在 `sync/state.json`，客户端不能指定扣费用户、上游地址或其他模型。当前计量覆盖小马问答与本地向量化；若以后接入远程 Embedding、额外的 OCR/摘要/Wiki 生成模型，需先让对应调用接入计量，不能假定 WeKnora 原生模型自动归账。

上线顺序：先部署支持 `ai_tokens` 的 Cloud，再通过 `deploy-weknora.py --sync-notes` 安装桥接，最后运行工作空间同步。安装器为 `18083` 仅放行 WeKnora 容器所在网桥/子网，并将宿主机网关 IP 加入 WeKnora 的 `SSRF_WHITELIST_EXTRA`，保留原有白名单；只有该配置改变时重建 WeKnora app，并等待健康检查后再同步。私有运行参数在 `library/runtime.json`，Compose 首次备份为 `compose.local.before-workspace-billing.json`。本次已按此顺序在测试服部署。

```powershell
python PonyNotes-Cloud-New/deploy/47.94.42.183/test-knowledge-billing.py
```

2026-11-11 测试服 Cloud/Worker 已更新为 `ponynotes/cloud:test-20261111-113726-17b84432`，包含当前工作区未提交的计费修改。发布记录与日志在 `/opt/ponynotes/releases/test-20261111-113726-17b84432/`，更新前配置和 PostgreSQL 备份在 `/opt/ponynotes/backups/test-20261111-113726-17b84432/`。Cloud 健康、真实登录/刷新、文件上传下载、WebSocket、现有验证码检查通过。随后更新桥接与 Nginx，迁移并同步 4 个空间知识库，错误数为 0；鉴权列表和文档接口返回 200，匿名列表返回 401，无效/无权空间的聊天请求分别返回 400/403。实际向量检索期间主人的 token 累计增加，问答次数保持不变。旧 Cloud 不支持 `ai_tokens`，回退时需一起恢复匹配的桥接脚本和同步状态。

同轮在线验收确认 43 条资料均为 `completed`。修正已有服务 Key 缺少 `manage_datasources` 导致的数据源列表 403 后，数据源、Wiki 列表及文档预览均返回 200；服务器 `deployment.json` 的 `knowledge_bridge` 记录脚本 SHA256、空间数量、索引状态和验证时间。

Flutter AI 欢迎页和聊天输入栏在「联网搜索」右侧增加「知识库检索」，默认关闭，点击后变为橙色。开启时先检索当前工作区中当前用户有权访问的已同步笔记，再将资料交给原有聊天模型回答；聊天记录保留原问题。没有相关资料时要求模型说明未找到相关笔记，检索失败时显示错误并可关闭开关重试。重新生成回答沿用原问题的检索开关，并重新检索。

检索使用独立的 Python 标准库服务，通过现有 Nginx 暴露 `POST /api/knowledge/search`。请求使用小马登录令牌和 `workspace_id`、`query`；服务通过 GoTrue 验证身份，并通过 Cloud API 检查当前工作区成员资格和页面可见性，再按工作区查找唯一知识库。成员无需个人 WeKnora 账号；不接受客户端指定用户、租户或知识库。WeKnora 密钥保留在服务器。返回最多 5 条去重来源，总计不超过 16000 字符，笔记引用保留小马打开链接。

已有云端笔记同步部署后，执行：

```powershell
python PonyNotes-Cloud-New/deploy/47.94.42.183/deploy-weknora.py --knowledge-search
python PonyNotes-Cloud-New/deploy/47.94.42.183/test-search-weknora.py
```

安装器创建 `ponynotes-weknora-search.service`，在小马 Docker 私有网关监听检索 `18082` 和内部向量计量 `18083`，并添加只允许对应 Docker 网桥/子网访问的 UFW 规则。公网通过原有 Cloud 端口访问检索，两个私有端口均不对公网开放；内部计量路由不经 Nginx 暴露。Nginx 修改前备份至 `/opt/ponynotes/nginx.conf.before-knowledge-search`，验证配置后重载。服务读取现有 `sync/state.json` 和 `sync-weknora.py`；同步定时器继续负责新增、修改及删除。

```sh
systemctl status ponynotes-weknora-search.service --no-pager
journalctl -u ponynotes-weknora-search.service -n 20 --no-pager
```

2026-11-06 验证：三个实际用户经 Nginx 均可检索到各自资料；跨用户工作区请求返回 403，匿名请求返回 401。离线回归覆盖伪造用户/知识库参数、页面权限过滤、来源去重和待同步用户。客户端 Rust DLL、Windows Debug 完整构建及 Flutter 开关/消息编码、debug/release/profile 环境配置测试通过，原有合并冲突和生成接口缺失已修复。检索后的生成继续使用小马聊天 API 的对话模型配置，与 WeKnora 模型配置独立；测试服对话模型已按下一节配置并通过上游调用验证，客户端实际回答由用户后续验收。

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

仅更新 Cloud 和同镜像的 Worker 时使用 `--cloud-only`。此模式只打包 Cloud 当前工作区，保留服务器现有 GoTrue/Java 镜像配置，不构建或重启这两个服务；仍执行数据库备份、Cloud 迁移和现有冒烟验证。知识库桥接脚本独立更新，随后执行 `deploy-weknora.py --knowledge-search`。

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
