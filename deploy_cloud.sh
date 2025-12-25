#!/bin/bash

# PonyNotes-Cloud-New 快速部署脚本
# 用途：构建Docker镜像、上传到服务器、部署并检查运行状态
# 日期：2025-11-26

set -e  # 遇到错误立即退出

# 颜色输出
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# 配置变量
PROJECT_DIR="/Users/kuncao/github.com/PonyNotes-IO/PonyNotes-Cloud-New"
SERVER_IP="8.152.101.166"
SERVER_USER="root"
SERVER_DOCKER_DIR="/root/docker-compose"
IMAGE_NAME="appflowyinc/appflowy_cloud:latest"
TAR_FILE="/tmp/appflowy_cloud.tar"
COMPOSE_FILE="docker-compose-dev.yml"

echo -e "${BLUE}========================================${NC}"
echo -e "${BLUE}PonyNotes-Cloud-New 自动部署开始${NC}"
echo -e "${BLUE}========================================${NC}"
echo ""

# 步骤1：本地构建Docker镜像
echo -e "${YELLOW}[步骤 1/7] 构建Docker镜像...${NC}"
cd "$PROJECT_DIR"
# docker compose --file "$COMPOSE_FILE" build appflowy_cloud

# if [ $? -eq 0 ]; then
#     echo -e "${GREEN}✅ Docker镜像构建成功${NC}"
# else
#     echo -e "${RED}❌ Docker镜像构建失败，退出部署${NC}"
#     exit 1
# fi
# echo ""

# 步骤2：导出镜像为tar文件
echo -e "${YELLOW}[步骤 2/7] 导出镜像到tar文件...${NC}"
docker save "$IMAGE_NAME" -o "$TAR_FILE"

if [ $? -eq 0 ]; then
    TAR_SIZE=$(du -h "$TAR_FILE" | cut -f1)
    echo -e "${GREEN}✅ 镜像导出成功，文件大小: $TAR_SIZE${NC}"
else
    echo -e "${RED}❌ 镜像导出失败${NC}"
    exit 1
fi
echo ""

# 步骤3：上传镜像到服务器
echo -e "${YELLOW}[步骤 3/7] 上传镜像到服务器...${NC}"
scp "$TAR_FILE" "${SERVER_USER}@${SERVER_IP}:${SERVER_DOCKER_DIR}/"

if [ $? -eq 0 ]; then
    echo -e "${GREEN}✅ 镜像上传成功${NC}"
else
    echo -e "${RED}❌ 镜像上传失败${NC}"
    exit 1
fi
echo ""

# 步骤4：停止原容器
echo -e "${YELLOW}[步骤 4/7] 停止服务器上的原容器...${NC}"
ssh "${SERVER_USER}@${SERVER_IP}" "cd ${SERVER_DOCKER_DIR} && docker compose --file ${COMPOSE_FILE} down"

if [ $? -eq 0 ]; then
    echo -e "${GREEN}✅ 原容器已停止${NC}"
else
    echo -e "${RED}❌ 停止容器失败${NC}"
    exit 1
fi
echo ""

# 步骤5：删除原镜像
echo -e "${YELLOW}[步骤 5/7] 删除服务器上的原镜像...${NC}"
ssh "${SERVER_USER}@${SERVER_IP}" "docker rmi ${IMAGE_NAME} 2>/dev/null || echo '镜像不存在，跳过删除'"
echo -e "${GREEN}✅ 原镜像处理完成${NC}"
echo ""

# 步骤6：导入新镜像
echo -e "${YELLOW}[步骤 6/7] 导入新镜像到服务器...${NC}"
ssh "${SERVER_USER}@${SERVER_IP}" "docker load -i ${SERVER_DOCKER_DIR}/appflowy_cloud.tar"

if [ $? -eq 0 ]; then
    echo -e "${GREEN}✅ 新镜像导入成功${NC}"
else
    echo -e "${RED}❌ 新镜像导入失败${NC}"
    exit 1
fi
echo ""

# 步骤7：启动新容器
echo -e "${YELLOW}[步骤 7/7] 启动新容器...${NC}"
ssh "${SERVER_USER}@${SERVER_IP}" "cd ${SERVER_DOCKER_DIR} && docker compose --file ${COMPOSE_FILE} up -d"

if [ $? -eq 0 ]; then
    echo -e "${GREEN}✅ 新容器启动成功${NC}"
else
    echo -e "${RED}❌ 新容器启动失败${NC}"
    exit 1
fi
echo ""

# 等待容器完全启动
echo -e "${BLUE}等待容器完全启动（5秒）...${NC}"
sleep 5
echo ""

# 检查运行状态
echo -e "${BLUE}========================================${NC}"
echo -e "${BLUE}检查部署状态${NC}"
echo -e "${BLUE}========================================${NC}"
echo ""

echo -e "${YELLOW}[状态检查 1/3] 查看容器运行状态...${NC}"
ssh "${SERVER_USER}@${SERVER_IP}" "docker ps --format 'table {{.Names}}\t{{.Status}}\t{{.Ports}}' | grep appflowy"
echo ""

echo -e "${YELLOW}[状态检查 2/3] 查看容器资源使用情况...${NC}"
ssh "${SERVER_USER}@${SERVER_IP}" "docker stats --no-stream --format 'table {{.Name}}\t{{.CPUPerc}}\t{{.MemUsage}}' | grep appflowy"
echo ""

echo -e "${YELLOW}[状态检查 3/3] 查看最近50行日志...${NC}"
echo -e "${BLUE}----------------------------------------${NC}"
ssh "${SERVER_USER}@${SERVER_IP}" "docker logs --tail 50 docker-compose-appflowy_cloud-1"
echo -e "${BLUE}----------------------------------------${NC}"
echo ""

# 清理本地tar文件
echo -e "${YELLOW}[清理] 删除本地tar文件...${NC}"
rm -f "$TAR_FILE"
echo -e "${GREEN}✅ 本地tar文件已清理${NC}"
echo ""

# 部署完成
echo -e "${GREEN}========================================${NC}"
echo -e "${GREEN}✅ 部署完成！${NC}"
echo -e "${GREEN}========================================${NC}"
echo ""

# 输出有用的命令提示
echo -e "${BLUE}📋 后续操作提示：${NC}"
echo ""
echo -e "${YELLOW}查看完整日志：${NC}"
echo "  ssh root@8.152.101.166 \"docker logs -f docker-compose-appflowy_cloud-1\""
echo ""
echo -e "${YELLOW}查看所有容器状态：${NC}"
echo "  ssh root@8.152.101.166 \"docker ps\""
echo ""
echo -e "${YELLOW}重启容器：${NC}"
echo "  ssh root@8.152.101.166 \"cd /root/docker-compose && docker compose --file docker-compose-dev.yml restart appflowy_cloud\""
echo ""
echo -e "${YELLOW}进入容器：${NC}"
echo "  ssh root@8.152.101.166 \"docker exec -it docker-compose-appflowy_cloud-1 /bin/bash\""
echo ""

# 健康检查
echo -e "${YELLOW}[健康检查] 测试AI聊天模型API...${NC}"
RESPONSE=$(curl -s http://8.152.101.166/api/ai/chat/models || echo "")

if [ -n "$RESPONSE" ]; then
    echo -e "${BLUE}API响应数据：${NC}"
    echo "$RESPONSE" | jq '.' 2>/dev/null || echo "$RESPONSE"

    # 检查响应是否包含预期的结构
    if echo "$RESPONSE" | grep -q '"code":0' && echo "$RESPONSE" | grep -q '"models":'; then
        MODEL_COUNT=$(echo "$RESPONSE" | grep -o '"id":"[^"]*"' | wc -l)
        echo -e "${GREEN}✅ AI聊天模型API健康检查通过 (返回了 $MODEL_COUNT 个模型)${NC}"
    else
        echo -e "${YELLOW}⚠️  API响应格式异常${NC}"
    fi
else
    echo -e "${YELLOW}⚠️  无法连接到AI聊天模型API（可能还在启动中，请等待30秒后手动检查）${NC}"
fi
echo ""

# 订阅计划接口测试
echo -e "${YELLOW}[健康检查] 测试订阅计划API...${NC}"
PLANS_RESPONSE=$(curl -s http://8.152.101.166/api/subscription/plans || echo "")

if [ -n "$PLANS_RESPONSE" ]; then
    echo -e "${BLUE}订阅计划API响应数据：${NC}"
    echo "$PLANS_RESPONSE" | jq '.' 2>/dev/null || echo "$PLANS_RESPONSE"

    # 检查响应是否包含预期的结构
    if echo "$PLANS_RESPONSE" | grep -q '"code":0' && echo "$PLANS_RESPONSE" | grep -q '"data":'; then
        PLAN_COUNT=$(echo "$PLANS_RESPONSE" | grep -o '"plan_code":"[^"]*"' | wc -l)
        echo -e "${GREEN}✅ 订阅计划API健康检查通过 (返回了 $PLAN_COUNT 个订阅计划)${NC}"
    else
        echo -e "${YELLOW}⚠️  订阅计划API响应格式异常${NC}"
    fi
else
    echo -e "${YELLOW}⚠️  无法连接到订阅计划API（可能还在启动中，请等待30秒后手动检查）${NC}"
fi
echo ""

echo -e "${GREEN}部署脚本执行完毕！${NC}"

