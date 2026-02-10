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
echo -e "${BLUE}开始构建 Docker 镜像...${NC}"

docker buildx build \
  -f Dockerfile \
  -t "${IMAGE_NAME}" \
  --build-arg DATABASE_URL="${DATABASE_URL}" \
  --build-arg CARGO_BUILD_JOBS=16 \
  --build-arg ENABLE_SCCACHE=true \
  --cache-from=type=local,src=/tmp/.buildx-cache \
  --cache-to=type=local,dest=/tmp/.buildx-cache,mode=max \
  --load .

BUILD_RESULT=$?
if [ $BUILD_RESULT -eq 0 ]; then
    echo -e "${GREEN}✅ Docker镜像构建成功${NC}"
else
    echo -e "${RED}❌ Docker镜像构建失败 (退出码: ${BUILD_RESULT})${NC}"
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
sleep 5
echo ""

# 测试API接口
echo -e "${YELLOW}[测试] 测试API接口...${NC}"
echo -e "${BLUE}----------------------------------------${NC}"

# 测试AI模型列表接口
echo -e "${BLUE}测试1: AI模型列表接口${NC}"
AI_MODELS_RESPONSE=$(curl -k -s https://xiaomabiji.com/api/ai/chat/models 2>&1)
if echo "$AI_MODELS_RESPONSE" | grep -q '"models"'; then
    echo -e "${GREEN}✅ AI模型列表接口正常${NC}"
    echo "模型数量: $(echo "$AI_MODELS_RESPONSE" | grep -o '"id"' | wc -l | tr -d ' ') 个"
else
    echo -e "${RED}❌ AI模型列表接口异常${NC}"
    echo "响应: ${AI_MODELS_RESPONSE:0:200}"
fi
echo ""

# 测试订阅计划接口
echo -e "${BLUE}测试2: 订阅计划接口${NC}"
SUBSCRIPTION_RESPONSE=$(curl -k -s https://xiaomabiji.com/api/subscription/plans 2>&1)
if echo "$SUBSCRIPTION_RESPONSE" | grep -q 'plans\|data'; then
    echo -e "${GREEN}✅ 订阅计划接口正常${NC}"
else
    echo -e "${RED}❌ 订阅计划接口异常${NC}"
    echo "响应: ${SUBSCRIPTION_RESPONSE:0:200}"
fi
echo ""

echo -e "${BLUE}----------------------------------------${NC}"
echo ""

# 检查运行状态
echo -e "${BLUE}容器运行状态:${NC}"
ssh -o StrictHostKeyChecking=no "${SERVER_USER}@${SERVER_IP}" "docker ps --format 'table {{.Names}}\t{{.Status}}\t{{.Ports}}' | grep -E 'appflowy|cloud' || true"

echo ""
echo -e "${GREEN}========================================${NC}"
echo -e "${GREEN}部署完成！${NC}"
echo -e "${GREEN}========================================${NC}"
echo ""
echo -e "${YELLOW}查看日志命令:${NC}"
echo "  ssh ${SERVER_USER}@${SERVER_IP} 'docker logs -f docker-compose-appflowy_cloud-1'"
echo ""

exit 0
