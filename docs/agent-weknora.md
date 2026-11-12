# 小马 Agent 的服务端执行链路

「问 AI」默认打开独立会话界面：桌面中间栏显示历史与「新建对话」，手机使用历史抽屉。新建不产生 AI 文档或「我的AI会话」空间，首页和会话内提供快问快答／智能推理选择。默认智能推理由 WeKnora Go 服务及其 Docker 沙箱执行，Flutter 不启动 Node/pi，也不执行本机命令。模型选择、积分、附件、笔记工具、写入审批和修改前后预览沿用现有能力。

原型阶段提供「旧对话（保留）」入口，既有 Chat 和 Agent 文档继续可用。旧 Agent 文档仍可切换快问快答／智能推理；服务端分别读取 `builtin-quick-answer`、`builtin-smart-reasoning` 两种内置配置。侧栏隐藏旧的 AI 空间入口，不删除既有文档或历史数据。

## 文件产物的保存

智能推理可以在会话沙箱内处理文件，将交付产物写入 `/workspace/output`。客户端从受鉴权的 Cloud 会话接口下载，完整下载后才作为本地文件呈现，点击文件卡片可打开。新界面的图片与文档统一由 Cloud 上传至 WeKnora 的 `sessions/{session_id}/attachments`，取得当前会话的临时文档 ID，再用 `attachment_ids` 提交问题；既有调用方的内联 `images` 参数仍保持可用。每轮最多四个附件、每个不超过 5 MB。

首页和会话页共用格式与大小检查。支持 PDF、Word（DOC/DOCX）、Excel（XLS/XLSX）、PowerPoint（PPT/PPTX）、EPUB、HTML/HTM/MHTML、XMind、常用文本与配置文件，以及 PNG/JPG/JPEG/WEBP/GIF/BMP/TIF/TIFF。Dart、Rust、JS/TS、Python、Java、C/C++、Go、Shell、SQL 等源代码在 WeKnora 中追加 `.txt` 后缀走完整文本解析，原始字节不变；Cloud 按附件 ID 恢复原始显示名。TIF、HTM、TEXT 仅规范化扩展名，大小写扩展名同样兼容。格式清单以客户端 `agent_attachments.dart` 和服务端 `attachments.rs` 为准。

会话附件保留原件，解析在 WeKnora 后台执行；Agent 可通过 `read_file` 的 `attachment://` 地址读取解析正文及页面图像，沙箱按会话挂载原件。上传指定实际 MIME 类型，视觉理解绑定当前用户选定的模型及凭据，覆盖内置配置的跨租户模型绑定。图片、扫描件及文档图表需要该模型支持视觉；纯文本模型无法识别图像。旧内联 `attachment_uploads` 没有临时文档 ID，已有历史无法自动补回原件，需重新上传。客户端用户消息显示文件名、大小和上传／解析状态，重新加载历史时从当前会话的临时附件记录更新状态；解析失败显示失败原因，过期附件提示重新上传。

2026-11-12 本地主流格式验证：19 个真实 DocReader 样例通过，覆盖 DOC/DOCX、XLS/XLSX、PPT/PPTX、普通与扫描 PDF、HTML/MHTML、EPUB、XMind 以及七种图片扩展名。正文类检查提取出的已知标记，图片与扫描件检查返回可读取图像；模型 OCR／图像理解尚未在真实商业模型上验收。原件字节、MIME、代码后缀适配和原名恢复另由 Rust HTTP fixtures 覆盖。测试资料与结果在工作区 `outputs/agent-attachment-fix/`。

工具调用按回答汇总为可展开的操作流程，保留参数、结果及笔记修改前后详情；审批继续逐次执行。会话首次打开滚动到最新位置，输出期间跟随底部；手动上翻后暂停跟随，点击「回到最新」恢复。Markdown 行内代码和代码块随明暗主题显示，使用主题强调色背景。

AI 生成的原始文件产物一律默认保存在应用数据目录的 `agent_artifacts/`，按服务器、账号、工作区和会话 ID 隔离；既有产物继续沿用原文档 ID。对话位于私有区或协作区都不自动开启产物云同步。复制到笔记或知识库的副本跟随目标自身的本地／云存储设置，原始产物仍保留本地；不能根据源会话的位置，或知识库的“私有／共享”访问权限，推断目标存储位置。当前笔记附件插入沿用目标文档的本地／云模式；产物库的「复制到笔记／知识库」目的地选择及导入尚未接入，知识库现有客户端接口只提供云端服务。

用户可主动开启当前会话的“产物云同步”，该明确选择会被记住。开启后，文件经现有 Cloud 文件存储接口上传，受工作区权限和存储限额约束；独立会话的元数据通过 POST `/conversations/{workspace_id}/{conversation_id}/artifacts` 保存到 `af_agent_conversations.artifacts`，其他设备从会话历史获取云副本。旧 Agent 文档继续在 View extra 中保存元数据。

新产物使用 `pn-agent-v1-` 命名空间，读取、上传和删除均校验来源。独立会话要求工作区访问权及会话所有者身份，工作区管理员也不能读取其他用户会话；旧文档产物沿祖先空间校验可见性。匿名下载被拒绝，下载响应禁止公共缓存，也不进入通用公开文件清单。关闭开关只影响后续自动上传，不删除已有云端副本。保存或上传失败时保留已落盘文件，并提供重试入口。

左侧「产物」菜单和问 AI 首页「全部产物」打开同一个本地产物库，支持文件名／来源会话搜索、预览、打开、另存为和返回来源对话。对话右上角「产物」打开当前会话的右侧预览栏，手机使用底部抽屉；点击回答中的文件卡片也进入该面板。图片可缩放、PDF 使用现有 pdfrx、Markdown 渲染排版，其余文本和代码按文本预览；DOCX、XLSX、PPTX 提取文字内容，保留打开原文件的入口。

本地产物目录按服务器、账号和工作区隔离，重启后不依赖模型余额读取。已有旧版文件在重新打开来源会话后补齐目录；新增文件在完整下载后建立记录。账号退出或工作区切换会撤下当前预览。「全部产物」只汇总本机已保存的文件，不会为了建库自动上传原始产物。

该开关控制文件产物的持久保存位置。对话历史继续由 WeKnora 保存；后端沙箱执行及 WeKnora 收集产物会产生服务端数据，不表示全部计算或数据只存在本机。

## 请求和工具执行

```text
Flutter → Cloud /api/ai/agent/conversations/{workspace_id}（列表／新建）
        → Cloud /api/ai/agent/conversations/{workspace_id}/{conversation_id}
        → WeKnora agent-chat（模型、工具循环、历史、压缩、引用）
        → Cloud /api/ai/agent/note-tools（Streamable HTTP MCP）
        → Redis 待处理调用 → 当前 Flutter 客户端
        → 原 McpTools / AgentNoteBridge → 笔记和数据库业务服务
```

客户端注册原有笔记工具定义，通过已登录的 Cloud API 领取操作。写入在 Flutter 的业务边界检查当前账号、工作区、工具开关、参数及审批；审批期间页面变化会拒绝继续修改。保留逐次确认、自动审批和完全开放三种模式。停止、断线、退出或连接被其他窗口替换时撤销待审批动作；已经交给业务服务的操作应查询实际结果，不能自动重试。

MCP 的随机凭据只在 Cloud、Redis 和 WeKnora 的加密凭据存储之间流转，不进入客户端对话。客户端每秒刷新连接租约，租约 15 秒后失效；每次注册变更授权代次，使旧调用不能恢复执行。工具请求不自动重放，客户端按调用 ID 执行一次。

## 发布前准备

1. 先发布 Cloud，执行 `migrations/20261111000000_weknora_agent_sessions.sql` 和 `migrations/20261112000000_agent_conversations.sql`，再发布客户端。新查询使用运行时 SQLx API，不需要增补 `.sqlx` 查询缓存。
2. 保留既有 `PONYNOTES_WEKNORA_ENABLED=true`、`PONYNOTES_WEKNORA_URL` 和用户同步映射。本轮实际验证测试服 WeKnora `v0.9.0`，运行版本提交 `f17ee8a`；内置配置和产物接口按对应源码核实。
3. WeKnora 必须能访问 Cloud 的 `/api/ai/agent/chat/completions` 和 `/api/ai/agent/note-tools`。默认 Base URL 为 `${PONYNOTES_WEKNORA_CLOUD_URL}/api/ai/agent`；网络不同时设置 `PONYNOTES_WEKNORA_MODEL_BASE_URL` 为同一 Cloud 的 Agent Base URL。只在 WeKnora 的 `SSRF_WHITELIST_EXTRA` 添加实际 Cloud 主机，不关闭 SSRF 检查。
4. Cloud 根据当前用户的内部模型列表建立独立模型配置，模型请求仍经过 Cloud → 用户自己的 sub2api Key，由 sub2api 计费。WeKnora 里存储的是有期限的 Cloud 模型令牌，每轮提交时更新；客户端不需要获得 sub2api Key。自定义模型的凭据会保存到用户自己的 WeKnora 模型配置中，部署应配置其加密存储；模型地址必须可由服务端访问，本机 loopback 模型不再能从远端引擎直连。
5. 联网查询需要在对应用户的 WeKnora 空间配置搜索服务；客户端的“联网”开关只决定本轮是否开放搜索。
6. WeKnora 开启 `WEKNORA_SANDBOX_DOCKER_ENABLED=true` 并可访问 Docker daemon。智能推理优先复用该用户已有的内置沙箱绑定或 Docker 配置；没有时在其独立租户创建会话级 Docker 配置。镜像默认 `wechatopenai/weknora-sandbox:v0.9.0`，可通过 Cloud 的 `PONYNOTES_WEKNORA_SANDBOX_IMAGE` 指定已部署的兼容镜像。管理员租户里的沙箱配置不会作为跨用户共享执行目录使用。

## 会话和资料范围

### 在提问时引用资料

在问 AI 首页或会话输入框输入 `@`，或点击引用按钮，可搜索「知识库／文件／笔记」，选择后显示可移除的标签。知识库包含当前用户可访问的私有及共享资源，文件列表展示各库上传的文件；笔记来自当前工作区 Cloud 目录的可见文档。新建会话首条消息也保留引用。每轮最多 20 项；运行中的补充指令仍只接受文字，附件和新引用需等待本轮结束。

GET `/api/ai/agent/conversations/{workspace_id}/references` 返回授权引用目录；POST 会话 `/messages` 新增可选 `references: [{"kind":"base|file|note","id":"UUID"}]`。未传此字段的调用沿用现有发送方式。Cloud 在每次提交时重新列出可访问资源，拒绝删除、撤权、类型不匹配的 ID，不接受客户端提供租户、知识库凭据或任意 URL。

知识库引用展开成授权文档 ID，`knowledge_base_ids` 继续为空，防止整个托管库绕过笔记可见性。当前工作区托管库里的上传文件可被选择，其他工作区托管笔记副本被排除。显式引用笔记通过当前用户 Cloud token 读取最新正文，作为资料附在本轮问题后；正文和问题合计超过 100000 字符时拒绝发送。笔记的 Worker 检索副本不代替最新正文。

会话 JSON 状态增加 `reference_items` 及 `input_contexts`，不需要新表或迁移。恢复历史时重新检查引用可见性，资料减少时沿用原有换会话规则；仅匹配 Cloud 生成的上下文标记才还原原问题和标签，普通用户文字不会被截断。文件回答来源通过 GET `/references/{knowledge_id}?page=1` 重新鉴权后读取解析正文，每页 25 个片段，客户端提供分页预览；笔记来源继续打开原笔记。

本轮代码尚未部署。客户端需要配套 Cloud 引用接口，旧测试服仅支持原发送接口。引用目录和共享资源的上游请求已用本地 HTTP fixtures 验证，文件正文预览分页已通过 Flutter 组件测试；真实 WeKnora／模型链路仍待联调。

本轮验证：Flutter Agent 测试 66 项、WeKnora 库测试 18 项通过；改动 Dart 文件分析通过；Docker Linux Rust 1.86 环境下 `SQLX_OFFLINE=true cargo check --locked --offline -p appflowy-cloud --lib` 通过。新增查询使用动态 SQL，未更改 SQLx 宏查询、数据表及 `.sqlx` 缓存。未运行 Cloud 全量集成测试，未生成客户端安装包。严格库 Clippy 的既有阻断为 `sync.rs` 的 `map_entry`，允许该项后检查通过。

`af_agent_conversations` 按用户、工作区登记独立会话的 ID、标题、更新时间及云产物元数据。GET `/api/ai/agent/conversations/{workspace_id}?offset=0` 每页返回 100 条当前用户会话；POST 接收客户端生成的 UUID 和标题，同一用户重复创建相同 ID 返回原记录，不覆盖标题，其他所有者占用返回 409。首次发送将默认「新对话」标题更新为提问前 40 个字符。

`af_weknora_agent_sessions` 继续按用户、工作区、来源 ID 保存模型、智能体及 WeKnora 会话映射；字段 `document_id` 对新架构存独立会话 ID，对旧入口仍存原 AI 文档 ID。独立会话先检查登记所有权，再构建有权限的工作区资料范围，不要求笔记目录存在同 ID 文档；旧入口继续检查文档可见性。会话记录由 WeKnora 保存，客户端通过 GET 加载历史，断流后用已有 assistant message ID 恢复，不重新发问题。运行中可补充指令或排队，停止接口由服务端传播取消。

独立会话按用户隔离，不随协作笔记共享；旧协作 AI 文档仍共享产物元数据，但其 WeKnora 上下文按用户隔离。

内置配置中的模型和重排模型绑定属于各自租户，业务侧使用当前用户选择的模型。知识范围、MCP 服务和记忆按小马笔记权限边界覆盖；跨会话检索与直接数据库操作仍按现有边界限制。智能推理开放 `ask_user`，技能选择使用当前用户沙箱中的全部已安装技能，允许 WeKnora 原生注册 `read_file` 与 `shell_exec`。快问快答不开放沙箱和笔记写工具。

### 知识库与笔记分别检索

知识库与笔记是独立业务。会话默认将用户当前可访问的知识库资料纳入 RAG；Cloud 仍展开为授权文档 ID，排除其他工作区托管副本和不可见笔记，不开放整个托管库。`@` 选择知识库或文件时收窄知识检索范围；只选择笔记不会关闭独立知识库检索。

快问快答先复用客户端现有 `search_notes` 和 `get_page`，从本地及已同步内容中检索并读取最多五篇笔记，每篇最多 4000 字符。`note_context`、`note_search_status` 随消息发送，Cloud 校验结构与大小，将笔记资料加入当前轮的 Agent 提示，并保留原问题用于 WeKnora 原生 RAG 的改写和检索；回答中的笔记来源打开原笔记。知识库无命中时，原生模型回退提示仍包含独立笔记资料；有笔记命中时关闭 FAQ 直接回答，避免跳过笔记内容。客户端读取前后复查账号与工作区，停止、退出或切换后不继续发送。检索关闭、超时或部分失败时明确提示，知识库检索仍继续；不会声称已检索全部笔记。快问快答不依赖远端笔记 MCP 初始化。

智能推理由模型按需调用独立知识检索和小马笔记 MCP 工具。笔记工具注册后立即开始续租，不再等待历史和模型设置加载完成，避免 15 秒租约在初始化期间过期；断开与撤权仍立即拒绝执行。

### 在对话内补充信息与继续任务

客户端展示 WeKnora 的原生 `ask_user` 和工具审批事件。追问在输入区原位显示，支持单选、多选、补充文字及文本／数字／选择表单，校验后提交，错误保留输入可重试。历史中的待回答问题可恢复，已回答的问题显示在时间线。POST 会话 `/prompt-response` 重新验证会话所有权、资料范围、消息和待回答问题的对应关系，再转发原生回答；暂停运行携带 `resume_pending_id` 恢复同一消息，不重新提问。追问期间仍可停止任务，Cloud 会解析对应的 assistant message ID 传给原生停止接口，避免空请求被上游拒绝。原有笔记写入审批与修改前后检查继续生效。

Cloud 根据当前可见页面筛选托管知识副本，只把授权文档 ID 交给 Agent，不开放整个知识库。若已有会话记录的资料范围出现减少，后续请求使用新会话，避免撤权后的旧上下文进入新一轮；旧服务端会话保留但不再向当前客户端返回。知识副本仍沿用原 Worker 的约五分钟扫描周期，刚编辑的内容可用客户端 `get_page` 等工具读取。

旧 pi 原生历史仍留在原本的本地应用数据目录，没有删除，也没有冒充新引擎的原生会话导入。原有 Chat 文档与 Chat 执行入口不变。退出客户端后，本机笔记工具不可执行。

## 本轮验证与测试服发布（2026-11-12）

- Flutter Agent 测试 60 项通过，覆盖本地产物、账号隔离、云同步、零积分恢复、独立会话分页／幂等创建、桌面／手机历史及刷新期间首页提问恢复。核心 Dart 静态分析无问题。产物与独立会话桌面／手机实际渲染已检查。
- `cargo test -p ponynotes-weknora --lib --offline` 的 14 项通过，包含独立会话授权范围及旧文档访问兼容。Cloud 在测试服完成 Linux release 构建；Windows Profile 客户端构建成功。
- 测试服 Cloud 已更新到 `ponynotes/cloud:agent-sessions-20261112`，Worker 和独立 WeKnora 保持原版本。WeKnora 不能解析业务网络的 `nginx`，故保留 `PONYNOTES_WEKNORA_MODEL_BASE_URL=http://172.19.0.1/api/ai/agent`，复用现有白名单和可达的宿主机反代入口。
- 固定测试模型实际驱动 Docker 沙箱生成 `acceptance.txt`；WeKnora 产物收集、Cloud 安全元数据与文件代理、云文件上传下载均验证通过。匿名与备用路由拒绝访问，公开列表不包含受保护产物。临时模型、会话、文档、文件、监听服务和测试网络规则已清理。
- 独立会话新建、列表、幂等重试及空历史通过，新建前后笔记目录一致；云产物元数据脱离笔记保存。合成的其他所有者会话对当前管理员不可见，历史和云文件返回 403，同 ID 接管返回 409。
- 既有业务冒烟验证通过，覆盖认证、文件、WebSocket、Java 后端与权限拒绝。测试管理员余额为零，未使用真实商业模型验收付费生成、计费及真实笔记编辑；这些仍需有余额账号完成。@ 引用、附件处理状态与自动化任务见根目录实施任务清单，尚未交付。
- 当前发布、备份、验收和回滚记录在工作区 `deploy/test-20261112-agent-sessions/DEPLOYMENT.md`。回滚保留新增表及数据；独立会话在旧客户端／旧 Cloud 上不会显示，需重新发布当前版本恢复。
