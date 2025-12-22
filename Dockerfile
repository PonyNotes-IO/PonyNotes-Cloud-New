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

ARG PROFILE="release"
ARG DATABASE_URL=postgres://postgres:password@postgres:5432/postgres
ENV DATABASE_URL=$DATABASE_URL

COPY --from=planner /app/recipe.json recipe.json
ENV CARGO_BUILD_JOBS=4
ENV CARGO_NET_GIT_FETCH_WITH_CLI=true
ENV CARGO_REGISTRIES_CRATES_IO_PROTOCOL=sparse
# Reduce memory usage during compilation
RUN echo "Building appflowy cloud with profile: ${PROFILE}"
RUN if [ "$PROFILE" = "release" ]; then \
      cargo chef cook --release --recipe-path recipe.json; \
    else \
      cargo chef cook --recipe-path recipe.json; \
    fi

COPY . .

# Try to prepare sqlx cache if database is available and sqlx-cli is installed
# This ensures all queries are cached before building with SQLX_OFFLINE
# Note: If .sqlx cache is incomplete, you should run `./prepare_sqlx_cache.sh` locally first
RUN if [ -n "$DATABASE_URL" ] && (command -v cargo-sqlx >/dev/null 2>&1 || cargo install sqlx-cli --no-default-features --features postgres --quiet 2>/dev/null); then \
      echo "Preparing SQLx cache with DATABASE_URL=$DATABASE_URL..."; \
      if cargo sqlx prepare --workspace 2>&1; then \
        echo "✓ SQLx cache prepared successfully"; \
      else \
        echo "⚠ Warning: SQLx prepare failed (database may not be accessible during build)"; \
        echo "⚠ Using existing cache. If build fails, run './prepare_sqlx_cache.sh' locally first."; \
      fi; \
    else \
      echo "⚠ Skipping SQLx cache preparation (database not available or sqlx-cli not installed)"; \
      echo "⚠ Using existing cache. If build fails, run './prepare_sqlx_cache.sh' locally first."; \
    fi

# Build the project.
# Attempt to prepare SQLx cache and, if successful, build with SQLX_OFFLINE in the same RUN.
# If prepare fails (e.g. DB not reachable during docker build), fall back to building without SQLX_OFFLINE.
RUN echo "Building with profile: ${PROFILE}" && \
    if [ "$PROFILE" = "release" ]; then \
      echo "Profile is release. Checking for sqlx cache or attempting prepare..."; \
      if cargo sqlx prepare --workspace >/dev/null 2>&1; then \
        echo "SQLx cache prepared successfully - building with SQLX_OFFLINE=true"; \
        SQLX_OFFLINE=true cargo build --release --bin appflowy_cloud; \
      elif [ -f ./sqlx-data.json ] || [ -d ./.sqlx ]; then \
        echo "Found existing sqlx cache in workspace - building with SQLX_OFFLINE=true"; \
        SQLX_OFFLINE=true cargo build --release --bin appflowy_cloud; \
      else \
        echo "ERROR: sqlx cache not found and 'cargo sqlx prepare' failed."; \
        echo "Please run 'cargo sqlx prepare --workspace' locally (or run ./prepare_sqlx_cache.sh) and include the generated 'sqlx-data.json' or '.sqlx' directory in the build context."; \
        echo "Aborting build to avoid obscure compile-time sqlx errors (see devops-docs for details)."; \
        exit 1; \
      fi; \
    else \
      echo "Profile is debug. Checking for sqlx cache or attempting prepare..."; \
      if cargo sqlx prepare --workspace >/dev/null 2>&1; then \
        echo "SQLx cache prepared successfully - building with SQLX_OFFLINE=true"; \
        SQLX_OFFLINE=true cargo build --bin appflowy_cloud; \
      elif [ -f ./sqlx-data.json ] || [ -d ./.sqlx ]; then \
        echo "Found existing sqlx cache in workspace - building with SQLX_OFFLINE=true"; \
        SQLX_OFFLINE=true cargo build --bin appflowy_cloud; \
      else \
        echo "ERROR: sqlx cache not found and 'cargo sqlx prepare' failed."; \
        echo "Please run 'cargo sqlx prepare --workspace' locally (or run ./prepare_sqlx_cache.sh) and include the generated 'sqlx-data.json' or '.sqlx' directory in the build context."; \
        echo "Aborting build to avoid obscure compile-time sqlx errors (see devops-docs for details)."; \
        exit 1; \
      fi; \
    fi

FROM debian:bookworm-slim AS runtime
WORKDIR /app
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
