# 测试服务器分享网页

`/share` 来自 `PonyNotes-Web-Desktop/xmbj-www-ui` 的 Nuxt 网页，需要独立的网页容器。只部署 Rust Cloud 时，该路径会落到 Cloud 并返回 404。网页通过 `ponynotes://note?...` 唤起客户端查看分享笔记。

这里保存 2026-11-05 在 `47.94.42.183` 验证通过的网页服务与 Nginx 配置。`compose.yaml` 是现有 `/opt/ponynotes/compose.yaml` 的补充文件，网页服务使用同一 Compose 项目网络，通过 `http://web_backend:8080` 访问 Java 接口。

## 部署

先备份服务器的 `nginx.conf`、`compose.override.yaml` 和 `deployment.json`。从网页项目构建或导入 Linux amd64 镜像 `ponynotes-web:1.0.0`，将本目录两个配置文件放到 `/opt/ponynotes/share-web/`，然后执行：

```sh
cd /opt/ponynotes
docker compose -f compose.yaml -f compose.override.yaml -f share-web/compose.yaml config --quiet
docker compose -f compose.yaml -f compose.override.yaml -f share-web/compose.yaml up -d --no-deps --pull never --wait web_frontend
cat share-web/nginx.conf > nginx.conf
docker compose exec -T nginx nginx -t
docker compose exec -T nginx nginx -s reload
```

Nginx 配置以文件绑定挂载，更新时保留原文件 inode。检查失败时恢复备份，再进行语法检查和 reload。网页服务的后续 Compose 操作需继续带上 `-f share-web/compose.yaml`。

已交付镜像归档位于网页项目的 `docker-artifacts/ponynotes-web-1.0.0/`，归档 SHA-256 为 `1b1eff9048e3d72aa2e6d0295a268342251aa6682ef6c418401e0e3e67877c36`；镜像与登录凭据不纳入本仓库。

## 验证记录

- 分享页、下载页及 Nuxt 脚本、样式和图片均返回 200。
- Chrome 中无页面脚本错误，分享 ID、空间 ID、权限和布局参数完整传入客户端链接。
- Cloud、认证和 Java 接口健康检查通过。
- 网页发布记录：`/opt/ponynotes/releases/test-20261105-164830-share-web`，同目录包含部署日志、浏览器验证记录及 `rollback.py`。
