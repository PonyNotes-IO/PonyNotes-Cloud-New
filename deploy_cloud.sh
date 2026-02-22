#!/bin/bash

# PonyNotes-Cloud-New 快速部署脚本
# 用途：本地构建Docker镜像并部署到服务器
# 日期：2026-02-09

set -e  # 遇到错误立即退出

# 颜色输出
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
CYAN='\033[0;36m'
NC='\033[0m' # No Color

# 配置变量
PROJECT_DIR="/Users/kuncao/github.com/PonyNotes-IO/PonyNotes-Cloud-New"
SERVER_IP="8.152.101.166"
SERVER_USER="root"
SERVER_DOCKER_DIR="/root/docker-compose"
IMAGE_NAME="appflowyinc/appflowy_cloud:latest"

echo -e "${CYAN}========================================${NC}"
echo -e "${CYAN}PonyNotes-Cloud-New 自动部署开始${NC}"
echo -e "${CYAN}========================================${NC}"
echo ""

# 从服务器获取真实的 DATABASE_URL
echo -e "${YELLOW}[步骤 1/5] 从服务器获取数据库配置...${NC}"
echo -e "${BLUE}连接到服务器获取 DATABASE_URL...${NC}"

# SSH 连接重试
MAX_RETRIES=3
RETRY_DELAY=5
RETRY_COUNT=0

while [ $RETRY_COUNT -lt $MAX_RETRIES ]; do
    REMOTE_ENV=$(ssh -o StrictHostKeyChecking=no -o ConnectTimeout=30 "${SERVER_USER}@${SERVER_IP}" "cat /root/docker-compose/.env 2>/dev/null" 2>&1) && break
    RETRY_COUNT=$((RETRY_COUNT + 1))
    if [ $RETRY_COUNT -lt $MAX_RETRIES ]; then
        echo -e "${YELLOW}SSH 连接失败，${RETRY_DELAY}秒后重试 (${RETRY_COUNT}/${MAX_RETRIES})...${NC}"
        sleep $RETRY_DELAY
    else
        echo -e "${RED}SSH 连接失败: ${REMOTE_ENV}${NC}"
        exit 1
    fi
done

# 提取 DATABASE_URL
DATABASE_URL=$(echo "$REMOTE_ENV" | grep '^DATABASE_URL=' | head -1 | sed 's/^DATABASE_URL=//' | tr -d '"' | tr -d '\r' | sed 's/^[[:space:]]*//;s/[[:space:]]*$//')

if [ -z "$DATABASE_URL" ]; then
    echo -e "${RED}❌ 无法从服务器获取 DATABASE_URL${NC}"
    echo -e "${RED}服务器返回内容:${NC}${REMOTE_ENV}"
    exit 1
fi
echo -e "${GREEN}✅ 已获取数据库配置 (长度: ${#DATABASE_URL})${NC}"
echo ""

# 构建Docker镜像
echo -e "${YELLOW}[步骤 2/5] 构建Docker镜像...${NC}"
cd "${PROJECT_DIR}"
export DOCKER_BUILDKIT=1

# 检测远程构建器（Intel Mac），避免 ARM64 上 QEMU 模拟的低效构建
BUILDER_ARG=""
REMOTE_BUILDER_NAME="intel-builder"
BUILDER_CONFIG="$HOME/.docker/ponynotes-builder.conf"

if [ -f "$BUILDER_CONFIG" ]; then
    source "$BUILDER_CONFIG"
    REMOTE_BUILDER_NAME="${BUILDER_NAME:-intel-builder}"
fi

if docker buildx inspect "$REMOTE_BUILDER_NAME" > /dev/null 2>&1; then
    BUILDER_ARG="--builder ${REMOTE_BUILDER_NAME}"
    echo -e "${GREEN}🚀 检测到远程构建器 '${REMOTE_BUILDER_NAME}'，将使用 Intel Mac 原生构建（预计10分钟）${NC}"
else
    LOCAL_ARCH=$(uname -m)
    if [ "$LOCAL_ARCH" = "arm64" ]; then
        echo -e "${YELLOW}⚠️  当前为 ARM64 架构，未检测到远程构建器，将使用 QEMU 模拟构建（较慢）${NC}"
        echo -e "${YELLOW}   提示：运行 scripts/setup_remote_builder.sh 配置 Intel Mac 远程构建器可大幅提速${NC}"
    else
        echo -e "${BLUE}当前为 x86_64 架构，直接原生构建${NC}"
    fi
fi

echo -e "${BLUE}开始构建 Docker 镜像...${NC}"
BUILD_START_TIME=$(date +%s)

docker buildx build \
  ${BUILDER_ARG} \
  -f Dockerfile \
  --platform linux/amd64 \
  -t "${IMAGE_NAME}" \
  --build-arg DATABASE_URL="${DATABASE_URL}" \
  --build-arg CARGO_BUILD_JOBS=16 \
  --build-arg ENABLE_SCCACHE=true \
  --load .

BUILD_RESULT=$?
BUILD_END_TIME=$(date +%s)
BUILD_DURATION=$(( BUILD_END_TIME - BUILD_START_TIME ))
BUILD_MINUTES=$(( BUILD_DURATION / 60 ))
BUILD_SECONDS=$(( BUILD_DURATION % 60 ))

if [ $BUILD_RESULT -eq 0 ]; then
    echo -e "${GREEN}✅ Docker镜像构建成功（耗时: ${BUILD_MINUTES}分${BUILD_SECONDS}秒）${NC}"
else
    echo -e "${RED}❌ Docker镜像构建失败 (退出码: ${BUILD_RESULT}，耗时: ${BUILD_MINUTES}分${BUILD_SECONDS}秒)${NC}"
    exit 1
fi
echo ""

# 导出镜像为tar文件（本地临时文件名可以随机，但服务器端固定为 appflowy_cloud.tar）
echo -e "${YELLOW}[步骤 3/5] 导出镜像为tar文件...${NC}"
TAR_FILE=$(mktemp).tar
docker save "${IMAGE_NAME}" -o "${TAR_FILE}"
TAR_SIZE=$(du -h "${TAR_FILE}" | cut -f1)
echo -e "${GREEN}✅ 镜像导出成功，文件大小: ${TAR_SIZE}${NC}"
echo ""

# 上传镜像到服务器，远端文件名固定为 appflowy_cloud.tar，和后面 docker load -i appflowy_cloud.tar 保持一致
echo -e "${YELLOW}[步骤 4/5] 上传镜像到服务器...${NC}"
scp -o StrictHostKeyChecking=no -o ConnectTimeout=60 "${TAR_FILE}" "${SERVER_USER}@${SERVER_IP}:${SERVER_DOCKER_DIR}/appflowy_cloud.tar"
rm -f "${TAR_FILE}"
echo -e "${GREEN}✅ 镜像上传成功${NC}"
echo ""

# 停止并部署新容器
echo -e "${YELLOW}[步骤 5/5] 部署到服务器...${NC}"

# 在服务器上执行部署命令
ssh -o StrictHostKeyChecking=no -o ConnectTimeout=30 "${SERVER_USER}@${SERVER_IP}" "cd '${SERVER_DOCKER_DIR}' && {
    echo '停止旧容器...'
    docker compose -f docker-compose-dev.yml down appflowy_cloud 2>/dev/null || true
    echo '删除旧镜像...'
    docker rmi '${IMAGE_NAME}' 2>/dev/null || true
    echo '导入新镜像...'
    docker load -i appflowy_cloud.tar
    echo '启动新容器...'
    docker compose -f docker-compose-dev.yml up -d appflowy_cloud
    rm -f appflowy_cloud.tar
    echo '部署命令执行完成'
    echo '等待容器启动...'
    sleep 10
    echo '检查容器状态...'
    docker compose -f docker-compose-dev.yml ps appflowy_cloud
}"

DEPLOY_RESULT=$?
if [ $DEPLOY_RESULT -eq 0 ]; then
    echo -e "${GREEN}✅ 部署成功${NC}"
else
    echo -e "${RED}❌ 部署失败 (退出码: ${DEPLOY_RESULT})${NC}"
    exit 1
fi
echo ""

# 等待服务完全启动
echo -e "${YELLOW}[等待] 等待服务完全启动...${NC}"
sleep 8
echo ""

# ============================================================
# API 接口全面测试
# ============================================================
API_BASE="http://${SERVER_IP}:8000"
GOTRUE_BASE="http://${SERVER_IP}:9999"
TEST_PHONE="13436574850"
TEST_PASSWORD='qwer1234!'

PASS_COUNT=0
FAIL_COUNT=0
TOTAL_COUNT=0

# 通用测试函数
# 参数: $1=测试名称 $2=请求方法 $3=URL $4=期望的JSON key $5...=额外curl参数
test_api() {
    local name="$1"
    local method="$2"
    local url="$3"
    local expect_key="$4"
    shift 4
    local extra_args=("$@")

    TOTAL_COUNT=$((TOTAL_COUNT + 1))
    local body
    body=$(curl -s -m 10 -X "$method" "$url" "${extra_args[@]}" 2>/dev/null)
    local http_code
    http_code=$(curl -s -m 10 -o /dev/null -w "%{http_code}" -X "$method" "$url" "${extra_args[@]}" 2>/dev/null)

    if [ "$http_code" -ge 200 ] 2>/dev/null && [ "$http_code" -lt 300 ] 2>/dev/null && echo "$body" | grep -q "\"${expect_key}\""; then
        PASS_COUNT=$((PASS_COUNT + 1))
        echo -e "  ${GREEN}✅ ${name}${NC} [HTTP ${http_code}]"
        local summary
        summary=$(echo "$body" | python3 -c "
import sys,json
try:
    d=json.load(sys.stdin)
    if 'data' in d:
        val=d['data']
        if isinstance(val, list):
            print(f'  返回 {len(val)} 条数据')
        elif isinstance(val, dict):
            keys=list(val.keys())[:5]
            print(f'  字段: {keys}')
        else:
            print(f'  值: {str(val)[:80]}')
    else:
        print(f'  keys: {list(d.keys())[:5]}')
except:
    pass
" 2>/dev/null)
        [ -n "$summary" ] && echo -e "    ${CYAN}${summary}${NC}"
    else
        FAIL_COUNT=$((FAIL_COUNT + 1))
        echo -e "  ${RED}❌ ${name}${NC} [HTTP ${http_code}]"
        echo -e "    ${RED}响应: ${body:0:150}${NC}"
    fi
}

echo -e "${CYAN}========================================${NC}"
echo -e "${CYAN}[测试] API 接口全面测试${NC}"
echo -e "${CYAN}========================================${NC}"
echo ""

# ─── 公共接口（无需登录）───
echo -e "${YELLOW}▶ 公共接口（无需认证）${NC}"

TOTAL_COUNT=$((TOTAL_COUNT + 1))
HEALTH=$(curl -s -m 10 "${API_BASE}/health" 2>/dev/null)
if [ "$HEALTH" = "OK" ]; then
    PASS_COUNT=$((PASS_COUNT + 1))
    echo -e "  ${GREEN}✅ 健康检查 /health${NC} [OK]"
else
    FAIL_COUNT=$((FAIL_COUNT + 1))
    echo -e "  ${RED}❌ 健康检查 /health${NC} [${HEALTH:0:50}]"
fi

test_api "服务器信息 /api/server" GET "${API_BASE}/api/server" "data"
test_api "订阅计划列表 /api/subscription/plans" GET "${API_BASE}/api/subscription/plans" "data"
test_api "GoTrue 健康检查" GET "${GOTRUE_BASE}/health" "version"

echo ""

# ─── 登录获取 Token ───
echo -e "${YELLOW}▶ 用户登录认证${NC}"

LOGIN_BODY=$(curl -s -m 10 -X POST "${GOTRUE_BASE}/token?grant_type=password" \
    -H "Content-Type: application/json" \
    -d "{\"phone\":\"${TEST_PHONE}\",\"password\":\"${TEST_PASSWORD}\"}" 2>/dev/null)
ACCESS_TOKEN=$(echo "$LOGIN_BODY" | python3 -c 'import sys,json;print(json.load(sys.stdin).get("access_token",""))' 2>/dev/null)
REFRESH_TOKEN=$(echo "$LOGIN_BODY" | python3 -c 'import sys,json;print(json.load(sys.stdin).get("refresh_token",""))' 2>/dev/null)

TOTAL_COUNT=$((TOTAL_COUNT + 1))
if [ -n "$ACCESS_TOKEN" ] && [ ${#ACCESS_TOKEN} -gt 20 ]; then
    PASS_COUNT=$((PASS_COUNT + 1))
    echo -e "  ${GREEN}✅ 手机号密码登录${NC} [token长度: ${#ACCESS_TOKEN}]"
    AUTH_HEADER=(-H "Authorization: Bearer ${ACCESS_TOKEN}")

    # ─── 用户接口 ───
    echo ""
    echo -e "${YELLOW}▶ 用户接口（需要认证）${NC}"
    test_api "用户资料 /api/user/profile" GET "${API_BASE}/api/user/profile" "data" "${AUTH_HEADER[@]}"
    test_api "用户工作区信息 /api/user/workspace" GET "${API_BASE}/api/user/workspace" "data" "${AUTH_HEADER[@]}"

    WORKSPACE_ID=$(curl -s -m 10 "${API_BASE}/api/user/workspace" "${AUTH_HEADER[@]}" 2>/dev/null | python3 -c 'import sys,json;print(json.load(sys.stdin)["data"]["visiting_workspace"]["workspace_id"])' 2>/dev/null)

    # ─── 工作区接口 ───
    echo ""
    echo -e "${YELLOW}▶ 工作区接口${NC}"
    test_api "工作区列表 /api/workspace" GET "${API_BASE}/api/workspace" "data" "${AUTH_HEADER[@]}"
    if [ -n "$WORKSPACE_ID" ]; then
        echo -e "    ${CYAN}使用工作区: ${WORKSPACE_ID}${NC}"
        test_api "工作区成员列表" GET "${API_BASE}/api/workspace/${WORKSPACE_ID}/member" "data" "${AUTH_HEADER[@]}"
        test_api "工作区设置" GET "${API_BASE}/api/workspace/${WORKSPACE_ID}/settings" "data" "${AUTH_HEADER[@]}"
        test_api "文档使用量" GET "${API_BASE}/api/workspace/${WORKSPACE_ID}/usage" "data" "${AUTH_HEADER[@]}"
    fi

    # ─── 订阅与账单接口 ───
    echo ""
    echo -e "${YELLOW}▶ 订阅与账单接口${NC}"
    test_api "当前订阅 /api/subscription/current" GET "${API_BASE}/api/subscription/current" "data" "${AUTH_HEADER[@]}"
    test_api "使用量 /api/subscription/usage" GET "${API_BASE}/api/subscription/usage" "data" "${AUTH_HEADER[@]}"
    test_api "订阅状态列表 /billing" GET "${API_BASE}/billing/api/v1/subscription-status" "data" "${AUTH_HEADER[@]}"
    if [ -n "$WORKSPACE_ID" ]; then
        test_api "工作区订阅状态" GET "${API_BASE}/billing/api/v1/subscription-status/${WORKSPACE_ID}" "data" "${AUTH_HEADER[@]}"
    fi

    # ─── 协作接口 ───
    echo ""
    echo -e "${YELLOW}▶ 协作接口${NC}"
    test_api "收到的协作邀请" GET "${API_BASE}/api/collab/me/received" "data" "${AUTH_HEADER[@]}"
    test_api "发出的协作邀请" GET "${API_BASE}/api/collab/me/sent" "data" "${AUTH_HEADER[@]}"

    # ─── 文件存储接口 ───
    if [ -n "$WORKSPACE_ID" ]; then
        echo ""
        echo -e "${YELLOW}▶ 文件存储接口${NC}"
        test_api "文件使用量" GET "${API_BASE}/api/file_storage/${WORKSPACE_ID}/usage" "data" "${AUTH_HEADER[@]}"
        test_api "文件列表" GET "${API_BASE}/api/file_storage/${WORKSPACE_ID}/blobs" "data" "${AUTH_HEADER[@]}"
    fi

    # ─── 共享接口 ───
    if [ -n "$WORKSPACE_ID" ]; then
        echo ""
        echo -e "${YELLOW}▶ 共享接口${NC}"
        test_api "共享文档列表" GET "${API_BASE}/api/sharing/workspace/${WORKSPACE_ID}/view" "data" "${AUTH_HEADER[@]}"
    fi

    # ─── Token 刷新 ───
    echo ""
    echo -e "${YELLOW}▶ Token 刷新${NC}"
    REFRESH_BODY=$(curl -s -m 10 -X POST "${GOTRUE_BASE}/token?grant_type=refresh_token" \
        -H "Content-Type: application/json" \
        -d "{\"refresh_token\":\"${REFRESH_TOKEN}\"}" 2>/dev/null)
    NEW_TOKEN=$(echo "$REFRESH_BODY" | python3 -c 'import sys,json;print(json.load(sys.stdin).get("access_token",""))' 2>/dev/null)
    TOTAL_COUNT=$((TOTAL_COUNT + 1))
    if [ -n "$NEW_TOKEN" ] && [ ${#NEW_TOKEN} -gt 20 ]; then
        PASS_COUNT=$((PASS_COUNT + 1))
        echo -e "  ${GREEN}✅ Token 刷新成功${NC} [新token长度: ${#NEW_TOKEN}]"
    else
        FAIL_COUNT=$((FAIL_COUNT + 1))
        echo -e "  ${RED}❌ Token 刷新失败${NC}"
    fi

else
    FAIL_COUNT=$((FAIL_COUNT + 1))
    echo -e "  ${RED}❌ 登录失败，跳过所有需要认证的接口测试${NC}"
    echo -e "    ${RED}响应: ${LOGIN_BODY:0:200}${NC}"
fi

# ─── 测试结果汇总 ───
echo ""
echo -e "${CYAN}========================================${NC}"
if [ $FAIL_COUNT -eq 0 ]; then
    echo -e "${GREEN}🎉 全部测试通过！ (${PASS_COUNT}/${TOTAL_COUNT})${NC}"
else
    echo -e "${YELLOW}📊 测试结果: ${GREEN}${PASS_COUNT} 通过${NC} / ${RED}${FAIL_COUNT} 失败${NC} / 共 ${TOTAL_COUNT} 项"
fi
echo -e "${CYAN}========================================${NC}"
echo ""

# 检查运行状态
echo -e "${BLUE}容器运行状态:${NC}"
ssh -o StrictHostKeyChecking=no "${SERVER_USER}@${SERVER_IP}" "docker ps --format 'table {{.Names}}\t{{.Status}}\t{{.Ports}}' | grep -E 'appflowy|cloud|gotrue|postgres' || true"

echo ""
echo -e "${GREEN}========================================${NC}"
echo -e "${GREEN}部署完成！${NC}"
echo -e "${GREEN}========================================${NC}"
echo ""
echo -e "${YELLOW}查看日志命令:${NC}"
echo "  ssh ${SERVER_USER}@${SERVER_IP} 'docker logs -f docker-compose-appflowy_cloud-1'"
echo ""

if [ $FAIL_COUNT -gt 0 ]; then
    exit 1
fi
exit 0
