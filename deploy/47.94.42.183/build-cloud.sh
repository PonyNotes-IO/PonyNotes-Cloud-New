#!/bin/bash
set -euo pipefail
cd /opt/ponynotes/source
export SQLX_OFFLINE=true
export CARGO_BUILD_JOBS=1
export CARGO_PROFILE_RELEASE_LTO=false
systemd-run --scope --quiet --unit=ponynotes-cloud-build -p MemoryHigh=3G -p MemoryMax=4G -p MemorySwapMax=4G -p CPUQuota=200% -p IOWeight=10 \
  cargo build --locked --release -p appflowy-cloud -p appflowy-worker --bin appflowy_cloud --bin appflowy_worker
cd /opt/ponynotes
docker build -f Dockerfile.cloud -t "ponynotes/cloud:${PONYNOTES_DEPLOY_TAG:-20261028}" .
