# 47.94.42.183 部署

服务器目录：`/opt/ponynotes`。Cloud、Worker、Java 后端和依赖均由 Docker Compose 管理，数据保存在命名卷，仅 Nginx 的 80 端口对外发布。

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
