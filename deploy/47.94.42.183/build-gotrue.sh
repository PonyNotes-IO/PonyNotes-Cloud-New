#!/bin/bash
set -euo pipefail
cd /opt/ponynotes/gotrue-source
GOPROXY=https://goproxy.cn GOMAXPROCS=2 CGO_ENABLED=0 go build -mod=readonly -p 2 -trimpath -buildvcs=false \
  -ldflags="-s -w -X github.com/supabase/auth/internal/utilities.Version=vponynotes-${PONYNOTES_DEPLOY_TAG:-73d0a1e-test-otp}" -o auth .
cd /opt/ponynotes
docker build -f Dockerfile.gotrue -t "ponynotes/gotrue:${PONYNOTES_DEPLOY_TAG:-73d0a1e-test-otp}" .
