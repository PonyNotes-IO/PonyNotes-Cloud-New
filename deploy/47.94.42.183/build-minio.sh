#!/bin/bash
set -euo pipefail
cd /opt/ponynotes
mkdir -p minio-source
tar -xzf minio-source.tar.gz --strip-components=1 -C minio-source
cd minio-source
GOPROXY=https://goproxy.cn GOMAXPROCS=2 CGO_ENABLED=0 go build -p 2 -trimpath -ldflags='-s -w' -o minio .
cd /opt/ponynotes
docker build -f Dockerfile.minio -t ponynotes/minio:2025-09-07 .
