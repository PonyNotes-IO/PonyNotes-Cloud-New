#!/bin/bash

# PonyNotes-Cloud-New 部署脚本
# x86_64 本地构建 → 导出镜像 → 上传服务器 → 部署 → HTTPS API 全量测试
# 日期：2026-02-23

set -e

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
CYAN='\033[0;36m'
NC='\033[0m'

PROJECT_DIR="/Users/kuncao/github.com/PonyNotes-IO/PonyNotes-Cloud-New"
SERVER_IP="8.152.101.166"
SERVER_USER="root"
SERVER_DOCKER_DIR="/root/docker-compose"
IMAGE_NAME="appflowyinc/appflowy_cloud:latest"
API_BASE="https://api.xiaomabiji.com"
GOTRUE_BASE="https://gotrue.xiaomabiji.com"

PASS_COUNT=0
FAIL_COUNT=0
TOTAL_COUNT=0
SCRIPT_START=$(date +%s)

# ─── 通用测试函数 ───
test_api() {
    local name="$1" method="$2" url="$3" expect_code="$4"
    shift 4
    local extra_args=("$@")
    TOTAL_COUNT=$((TOTAL_COUNT + 1))

    local resp_file
    resp_file=$(mktemp)
    local http_code
    http_code=$(curl -sk -m 15 -o "$resp_file" -w "%{http_code}" -X "$method" "$url" "${extra_args[@]}" 2>/dev/null || echo "000")
    local body
    body=$(cat "$resp_file" 2>/dev/null)
    rm -f "$resp_file"

    if [ "$http_code" = "$expect_code" ]; then
        PASS_COUNT=$((PASS_COUNT + 1))
        echo -e "  ${GREEN}✅ ${name}${NC} [${http_code}]"
    else
        FAIL_COUNT=$((FAIL_COUNT + 1))
        echo -e "  ${RED}❌ ${name}${NC} [期望:${expect_code} 实际:${http_code}]"
        echo -e "    ${RED}${body:0:120}${NC}"
    fi
}

# 登录并返回 access_token，参数：$1=账号 $2=密码 $3=登录方式(phone|email)
do_login() {
    local account="$1" password="$2" login_type="$3"
    local field="phone"
    [ "$login_type" = "email" ] && field="email"
    local body
    body=$(curl -sk -m 15 -X POST "${GOTRUE_BASE}/token?grant_type=password" \
        -H "Content-Type: application/json" \
        -d "{\"${field}\":\"${account}\",\"password\":\"${password}\"}" 2>/dev/null)
    echo "$body" | python3 -c 'import sys,json;print(json.load(sys.stdin).get("access_token",""))' 2>/dev/null
}

echo -e "${CYAN}════════════════════════════════════════${NC}"
echo -e "${CYAN}  PonyNotes-Cloud 构建 · 部署 · 测试${NC}"
echo -e "${CYAN}════════════════════════════════════════${NC}"
echo ""

# ══════════════════════════════════════
# 步骤 1：获取数据库连接字符串
# ══════════════════════════════════════
echo -e "${YELLOW}[1/5] 获取数据库配置...${NC}"
RETRY=0
while [ $RETRY -lt 3 ]; do
    REMOTE_ENV=$(ssh -o StrictHostKeyChecking=no -o ConnectTimeout=30 "${SERVER_USER}@${SERVER_IP}" "cat ${SERVER_DOCKER_DIR}/.env 2>/dev/null" 2>&1) && break
    RETRY=$((RETRY + 1))
    [ $RETRY -lt 3 ] && echo -e "${YELLOW}  重试 ${RETRY}/3...${NC}" && sleep 5
done
DATABASE_URL=$(echo "$REMOTE_ENV" | grep '^DATABASE_URL=' | head -1 | sed 's/^DATABASE_URL=//' | tr -d '"' | tr -d '\r' | sed 's/^[[:space:]]*//;s/[[:space:]]*$//')
if [ -z "$DATABASE_URL" ]; then
    echo -e "${RED}❌ 获取 DATABASE_URL 失败${NC}"
    exit 1
fi
echo -e "${GREEN}  ✅ 已获取 (长度:${#DATABASE_URL})${NC}"

# ══════════════════════════════════════
# 步骤 2：构建 Docker 镜像（x86_64 原生）
# ══════════════════════════════════════
echo ""
echo -e "${YELLOW}[2/5] 构建 Docker 镜像 (x86_64 原生)...${NC}"
cd "${PROJECT_DIR}"
export DOCKER_BUILDKIT=1
T0=$(date +%s)

docker build \
  -f Dockerfile \
  -t "${IMAGE_NAME}" \
  --build-arg DATABASE_URL="${DATABASE_URL}" \
  --build-arg CARGO_BUILD_JOBS=16 \
  --build-arg ENABLE_SCCACHE=true \
  .

T1=$(date +%s)
echo -e "${GREEN}  ✅ 构建成功 ($(( (T1-T0)/60 ))分$(( (T1-T0)%60 ))秒)${NC}"

# ══════════════════════════════════════
# 步骤 3：导出镜像
# ══════════════════════════════════════
echo ""
echo -e "${YELLOW}[3/5] 导出镜像...${NC}"
TAR_FILE=$(mktemp).tar
docker save "${IMAGE_NAME}" -o "${TAR_FILE}"
TAR_SIZE=$(du -h "${TAR_FILE}" | cut -f1)
echo -e "${GREEN}  ✅ 导出完成 (${TAR_SIZE})${NC}"

# ══════════════════════════════════════
# 步骤 4：上传到服务器
# ══════════════════════════════════════
echo ""
echo -e "${YELLOW}[4/5] 上传镜像到服务器...${NC}"
scp -o StrictHostKeyChecking=no -o ConnectTimeout=60 "${TAR_FILE}" "${SERVER_USER}@${SERVER_IP}:${SERVER_DOCKER_DIR}/appflowy_cloud.tar"
rm -f "${TAR_FILE}"
echo -e "${GREEN}  ✅ 上传完成${NC}"

# ══════════════════════════════════════
# 步骤 5：服务器部署
# ══════════════════════════════════════
echo ""
echo -e "${YELLOW}[5/5] 服务器部署...${NC}"
ssh -o StrictHostKeyChecking=no -o ConnectTimeout=30 "${SERVER_USER}@${SERVER_IP}" "cd '${SERVER_DOCKER_DIR}' && \
    docker compose -f docker-compose-dev.yml down appflowy_cloud 2>/dev/null || true && \
    docker rmi '${IMAGE_NAME}' 2>/dev/null || true && \
    docker load -i appflowy_cloud.tar && \
    docker compose -f docker-compose-dev.yml up -d appflowy_cloud && \
    rm -f appflowy_cloud.tar && \
    echo 'done'"
echo -e "${GREEN}  ✅ 部署完成，等待启动...${NC}"
sleep 15

# 验证容器运行
echo -e "${BLUE}  容器状态:${NC}"
ssh -o StrictHostKeyChecking=no "${SERVER_USER}@${SERVER_IP}" "docker ps --format '  {{.Names}}\t{{.Status}}' | grep -E 'appflowy_cloud|gotrue|postgres' || true"
echo ""

# ══════════════════════════════════════════════════════════════
# API 全量测试（通过 https://api.xiaomabiji.com）
# ══════════════════════════════════════════════════════════════
echo -e "${CYAN}════════════════════════════════════════${NC}"
echo -e "${CYAN}  HTTPS API 全量测试${NC}"
echo -e "${CYAN}════════════════════════════════════════${NC}"
echo ""

# ─── 公共接口 ───
echo -e "${YELLOW}▶ 公共接口（无需认证）${NC}"
test_api "健康检查 /health" GET "${API_BASE}/health" "200"
test_api "服务器信息" GET "${API_BASE}/api/server" "200"
test_api "GoTrue 健康检查" GET "${GOTRUE_BASE}/health" "200"

# ─── 测试单个用户的所有接口 ───
test_user_apis() {
    local label="$1" account="$2" password="$3" login_type="$4"
    echo ""
    echo -e "${CYAN}── ${label} (${account}) ──${NC}"

    # 登录
    echo -e "${YELLOW}▶ 登录${NC}"
    local token
    token=$(do_login "$account" "$password" "$login_type")
    TOTAL_COUNT=$((TOTAL_COUNT + 1))
    if [ -n "$token" ] && [ ${#token} -gt 20 ]; then
        PASS_COUNT=$((PASS_COUNT + 1))
        echo -e "  ${GREEN}✅ 登录成功${NC} [token:${#token}字符]"
    else
        FAIL_COUNT=$((FAIL_COUNT + 1))
        echo -e "  ${RED}❌ 登录失败，跳过后续测试${NC}"
        return
    fi
    local AUTH=(-H "Authorization: Bearer ${token}")

    # 用户接口
    echo -e "${YELLOW}▶ 用户接口${NC}"
    test_api "用户资料" GET "${API_BASE}/api/user/profile" "200" "${AUTH[@]}"
    test_api "用户工作区" GET "${API_BASE}/api/user/workspace" "200" "${AUTH[@]}"

    # 获取工作区 ID
    local WS_ID
    WS_ID=$(curl -sk -m 10 "${API_BASE}/api/user/workspace" "${AUTH[@]}" 2>/dev/null | python3 -c 'import sys,json;print(json.load(sys.stdin)["data"]["visiting_workspace"]["workspace_id"])' 2>/dev/null)
    if [ -z "$WS_ID" ]; then
        echo -e "  ${RED}无法获取工作区ID，跳过工作区相关测试${NC}"
        return
    fi
    echo -e "  ${BLUE}工作区: ${WS_ID}${NC}"

    # 工作区接口
    echo -e "${YELLOW}▶ 工作区接口${NC}"
    test_api "工作区列表" GET "${API_BASE}/api/workspace" "200" "${AUTH[@]}"
    test_api "工作区成员" GET "${API_BASE}/api/workspace/${WS_ID}/member" "200" "${AUTH[@]}"
    test_api "工作区设置" GET "${API_BASE}/api/workspace/${WS_ID}/settings" "200" "${AUTH[@]}"
    test_api "文档使用量" GET "${API_BASE}/api/workspace/${WS_ID}/usage" "200" "${AUTH[@]}"

    # 发布相关接口
    echo -e "${YELLOW}▶ 发布接口${NC}"
    test_api "发布列表(workspace)" GET "${API_BASE}/api/workspace/${WS_ID}/published-info" "200" "${AUTH[@]}"
    test_api "发布列表(全局)" GET "${API_BASE}/api/workspace/published-info/all" "200" "${AUTH[@]}"
    test_api "发布命名空间" GET "${API_BASE}/api/workspace/${WS_ID}/publish-namespace" "200" "${AUTH[@]}"

    # 发布接收接口（POST 空参数验证路由存在，期望 400 而非 405）
    local RECEIVE_CODE
    RECEIVE_CODE=$(curl -sk -m 10 -o /dev/null -w "%{http_code}" -X POST "${API_BASE}/api/workspace/published/receive" \
        -H "Content-Type: application/json" "${AUTH[@]}" \
        -d '{"published_view_id":"00000000-0000-0000-0000-000000000000","dest_workspace_id":"00000000-0000-0000-0000-000000000000","dest_view_id":"00000000-0000-0000-0000-000000000001"}' 2>/dev/null)
    TOTAL_COUNT=$((TOTAL_COUNT + 1))
    if [ "$RECEIVE_CODE" != "405" ]; then
        PASS_COUNT=$((PASS_COUNT + 1))
        echo -e "  ${GREEN}✅ 发布接收路由可达${NC} [${RECEIVE_CODE}]"
    else
        FAIL_COUNT=$((FAIL_COUNT + 1))
        echo -e "  ${RED}❌ 发布接收路由不存在(405)，需要重新部署${NC}"
    fi

    # 文件夹/视图接口
    echo -e "${YELLOW}▶ 文件夹与视图接口${NC}"
    test_api "文件夹结构" GET "${API_BASE}/api/workspace/${WS_ID}/folder" "200" "${AUTH[@]}"
    test_api "最近文档" GET "${API_BASE}/api/workspace/${WS_ID}/recent" "200" "${AUTH[@]}"
    test_api "收藏文档" GET "${API_BASE}/api/workspace/${WS_ID}/favorite" "200" "${AUTH[@]}"
    test_api "回收站" GET "${API_BASE}/api/workspace/${WS_ID}/trash" "200" "${AUTH[@]}"

    # 协作接口
    echo -e "${YELLOW}▶ 协作接口${NC}"
    test_api "收到的协作邀请" GET "${API_BASE}/api/collab/me/received" "200" "${AUTH[@]}"
    test_api "发出的协作邀请" GET "${API_BASE}/api/collab/me/sent" "200" "${AUTH[@]}"

    # 共享接口
    echo -e "${YELLOW}▶ 共享接口${NC}"
    test_api "共享文档列表" GET "${API_BASE}/api/sharing/workspace/${WS_ID}/view" "200" "${AUTH[@]}"

    # 文件存储
    echo -e "${YELLOW}▶ 文件存储接口${NC}"
    test_api "文件使用量" GET "${API_BASE}/api/file_storage/${WS_ID}/usage" "200" "${AUTH[@]}"
    test_api "文件列表" GET "${API_BASE}/api/file_storage/${WS_ID}/blobs" "200" "${AUTH[@]}"

    # 数据库接口
    echo -e "${YELLOW}▶ 数据库接口${NC}"
    test_api "数据库列表" GET "${API_BASE}/api/workspace/${WS_ID}/database" "200" "${AUTH[@]}"

    # 快捷笔记
    echo -e "${YELLOW}▶ 快捷笔记接口${NC}"
    test_api "快捷笔记列表" GET "${API_BASE}/api/workspace/${WS_ID}/quick-note" "200" "${AUTH[@]}"

    # 订阅接口
    echo -e "${YELLOW}▶ 订阅接口${NC}"
    test_api "当前订阅" GET "${API_BASE}/api/subscription/current" "200" "${AUTH[@]}"
    test_api "使用量" GET "${API_BASE}/api/subscription/usage" "200" "${AUTH[@]}"

    # Token 刷新
    echo -e "${YELLOW}▶ Token 刷新${NC}"
    local refresh_token
    refresh_token=$(curl -sk -m 10 -X POST "${GOTRUE_BASE}/token?grant_type=password" \
        -H "Content-Type: application/json" \
        -d "{\"${login_type}\":\"${account}\",\"password\":\"${password}\"}" 2>/dev/null \
        | python3 -c 'import sys,json;print(json.load(sys.stdin).get("refresh_token",""))' 2>/dev/null)
    if [ -n "$refresh_token" ]; then
        local new_token
        new_token=$(curl -sk -m 10 -X POST "${GOTRUE_BASE}/token?grant_type=refresh_token" \
            -H "Content-Type: application/json" \
            -d "{\"refresh_token\":\"${refresh_token}\"}" 2>/dev/null \
            | python3 -c 'import sys,json;print(json.load(sys.stdin).get("access_token",""))' 2>/dev/null)
        TOTAL_COUNT=$((TOTAL_COUNT + 1))
        if [ -n "$new_token" ] && [ ${#new_token} -gt 20 ]; then
            PASS_COUNT=$((PASS_COUNT + 1))
            echo -e "  ${GREEN}✅ Token 刷新成功${NC}"
        else
            FAIL_COUNT=$((FAIL_COUNT + 1))
            echo -e "  ${RED}❌ Token 刷新失败${NC}"
        fi
    fi
}

# ─── 账号1：手机号登录 ───
test_user_apis "账号1" "13436574850" 'Qwer1234!' "phone"

# ─── 账号2：邮箱登录 ───
test_user_apis "账号2" "87103978@qq.com" 'Qwer1234!' "email"

# ══════════════════════════════════════
# 测试结果汇总
# ══════════════════════════════════════
SCRIPT_END=$(date +%s)
TOTAL_SEC=$((SCRIPT_END - SCRIPT_START))
echo ""
echo -e "${CYAN}════════════════════════════════════════${NC}"
if [ $FAIL_COUNT -eq 0 ]; then
    echo -e "${GREEN}  全部通过！ ${PASS_COUNT}/${TOTAL_COUNT} (耗时 $((TOTAL_SEC/60))分$((TOTAL_SEC%60))秒)${NC}"
else
    echo -e "${YELLOW}  通过: ${GREEN}${PASS_COUNT}${NC} / 失败: ${RED}${FAIL_COUNT}${NC} / 共 ${TOTAL_COUNT} (耗时 $((TOTAL_SEC/60))分$((TOTAL_SEC%60))秒)"
fi
echo -e "${CYAN}════════════════════════════════════════${NC}"
echo ""
echo -e "${BLUE}查看日志: ssh ${SERVER_USER}@${SERVER_IP} 'docker logs -f docker-compose-appflowy_cloud-1'${NC}"
echo ""

[ $FAIL_COUNT -gt 0 ] && exit 1
exit 0
