# Using cargo-chef to manage Rust build cache effectively
FROM lukemathwalker/cargo-chef:latest-rust-1.86 as chef

WORKDIR /app
RUN apt update && apt install lld clang -y

FROM chef as planner
COPY . .
# Compute a lock-like file for our project
RUN cargo chef prepare --recipe-path recipe.json

FROM chef as builder

# Update package lists and install protobuf-compiler along with other build dependencies
RUN apt update && apt install -y protobuf-compiler lld clang

# Install sccache to speed up repeated Rust compilations and configure wrapper.
# sccache will cache rustc outputs; we also expose SCCACHE_DIR for BuildKit cache mounting.
RUN cargo install --locked sccache && echo "sccache installed successfully" || echo "sccache install failed, will skip cache"
# 检查 sccache 是否安装成功
RUN if [ -x "/root/.cargo/bin/sccache" ]; then echo "sccache found at /root/.cargo/bin/sccache"; sccache --version; else echo "sccache not installed"; fi
## Do not enable RUSTC_WRAPPER during cargo-chef cook step to avoid wrapper interfering with recipe probe.
ENV RUSTC_WRAPPER=""
ENV SCCACHE_DIR=/sccache

ARG PROFILE="release"
ARG DATABASE_URL="postgres://invalid:invalid@invalid:5432/invalid"
ARG ENABLE_SCCACHE="false"
ENV DATABASE_URL="${DATABASE_URL}"
ENV ENABLE_SCCACHE="${ENABLE_SCCACHE}"

COPY --from=planner /app/recipe.json recipe.json
ENV CARGO_BUILD_JOBS=4
ENV CARGO_NET_GIT_FETCH_WITH_CLI=true
ENV CARGO_REGISTRIES_CRATES_IO_PROTOCOL=sparse
# 禁用增量编译以减少内存占用（Docker 构建不需要增量）
ENV CARGO_INCREMENTAL=0
# Reduce memory usage during compilation
RUN echo "Building appflowy cloud with profile: ${PROFILE}"
# Use BuildKit cache mounts to persist cargo registry/git and sccache between builds.
# This requires DOCKER_BUILDKIT=1 and buildx; mounts significantly speed up subsequent builds.
RUN --mount=type=cache,target=/root/.cargo/registry \
    --mount=type=cache,target=/root/.cargo/git \
    --mount=type=cache,target=/sccache \
    if [ "$PROFILE" = "release" ]; then \
      cargo chef cook --release --recipe-path recipe.json; \
    else \
      cargo chef cook --recipe-path recipe.json; \
    fi

COPY . .

# Build the project.
# IMPORTANT: We use SQLX_OFFLINE=true to use cached query validation.
# This avoids needing database access during Docker build time.
# The .sqlx directory with query metadata must be committed to version control.
ENV SQLX_OFFLINE=true
# 配置 sccache，但让它自己查找 rustc
ENV SCCACHE_DIR=/sccache
RUN --mount=type=cache,target=/root/.cargo/registry \
    --mount=type=cache,target=/root/.cargo/git \
    --mount=type=cache,target=/sccache \
    echo "Building with profile: ${PROFILE} (SQLX_OFFLINE=true)" && \
    # 查找 rustc 和 sccache 的实际路径
    RUSTC_PATH=$(command -v rustc 2>/dev/null || echo "") && \
    if [ -n "$RUSTC_PATH" ] && [ -x "/root/.cargo/bin/sccache" ]; then \
      echo "Using sccache at /root/.cargo/bin/sccache with rustc at $RUSTC_PATH"; \
      export RUSTC_WRAPPER=/root/.cargo/bin/sccache; \
    else \
      echo "sccache not available, skipping"; \
      export RUSTC_WRAPPER=""; \
    fi && \
    if [ "$PROFILE" = "release" ]; then \
      echo "Building release binary"; \
      cargo build --release --bin appflowy_cloud; \
    else \
      echo "Building debug binary"; \
      cargo build --bin appflowy_cloud; \
    fi

FROM debian:bookworm-slim AS runtime
WORKDIR /app
# 使用阿里云镜像源解决国内网络问题
RUN sed -i 's/deb.debian.org/mirrors.aliyun.com/g' /etc/apt/sources.list.d/debian.sources 2>/dev/null || \
    sed -i 's/deb.debian.org/mirrors.aliyun.com/g' /etc/apt/sources.list 2>/dev/null || true
RUN apt-get update -y \
  && apt-get install -y --no-install-recommends openssl ca-certificates curl \
  && update-ca-certificates \
  # Clean up
  && apt-get autoremove -y \
  && apt-get clean -y \
  && rm -rf /var/lib/apt/lists/*

# Copy the binary from the appropriate target directory
ARG PROFILE="release"
RUN echo "Building with profile: ${PROFILE}"
RUN if [ "$PROFILE" = "release" ]; then \
      echo "Using release binary"; \
    else \
      echo "Using debug binary"; \
    fi
COPY --from=builder /app/target/$PROFILE/appflowy_cloud /usr/local/bin/appflowy_cloud
ENV APP_ENVIRONMENT production
ENV RUST_BACKTRACE 1

ARG APPFLOWY_APPLICATION_PORT
ARG PORT
ENV PORT=${APPFLOWY_APPLICATION_PORT:-${PORT:-8000}}
EXPOSE $PORT

CMD ["appflowy_cloud"]
