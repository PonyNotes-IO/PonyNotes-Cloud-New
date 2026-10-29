#!/bin/bash
set -euo pipefail
cd /opt/ponynotes/source
export SQLX_OFFLINE=true
export CARGO_BUILD_JOBS=2
export CARGO_PROFILE_RELEASE_LTO=false
cargo build --locked --release -p appflowy-cloud -p appflowy-worker --bin appflowy_cloud --bin appflowy_worker
cd /opt/ponynotes
docker build -f Dockerfile.cloud -t "ponynotes/cloud:${PONYNOTES_DEPLOY_TAG:-20261028}" .
