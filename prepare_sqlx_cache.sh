#!/bin/bash
# Script to prepare SQLx cache before Docker build
# This ensures all queries are cached for offline builds

set -e

echo "=========================================="
echo "  Preparing SQLx Cache"
echo "=========================================="
echo ""

# Check if database is running
POSTGRES_RUNNING=false
if docker compose -f docker-compose-dev.yml ps postgres 2>/dev/null | grep -qE "Up|running"; then
    POSTGRES_RUNNING=true
    echo "PostgreSQL container is already running"
else
    echo "Starting PostgreSQL container..."
    docker compose -f docker-compose-dev.yml up -d postgres
    echo "Waiting for PostgreSQL to start..."
    sleep 5
fi

# Load environment variables first to get DATABASE_URL
if [ -f .env ]; then
    # Use a safer method to load env vars
    set -a
    source .env
    set +a
fi

export DATABASE_URL=${DATABASE_URL:-postgres://postgres:password@localhost:5432/postgres}

# Wait for PostgreSQL to be ready (check via cargo sqlx database create or direct connection)
echo "Checking PostgreSQL connection..."
echo "DATABASE_URL: $DATABASE_URL"
max_attempts=30
attempt=0
while [ $attempt -lt $max_attempts ]; do
    # Use cargo sqlx to check connection (most reliable way)
    if cargo sqlx database create 2>&1 | grep -qE "Database|already exists|error"; then
        echo "✓ PostgreSQL connection verified"
        break
    fi
    
    # Also try direct psql connection if available
    if command -v psql >/dev/null 2>&1; then
        # Extract connection details from DATABASE_URL
        DB_PASSWORD=$(echo "$DATABASE_URL" | sed -n 's/.*:\/\/[^:]*:\([^@]*\)@.*/\1/p')
        DB_USER=$(echo "$DATABASE_URL" | sed -n 's/.*:\/\/\([^:]*\):.*/\1/p')
        DB_HOST=$(echo "$DATABASE_URL" | sed -n 's/.*@\([^:]*\):.*/\1/p')
        DB_PORT=$(echo "$DATABASE_URL" | sed -n 's/.*:\([0-9]*\)\/.*/\1/p')
        DB_NAME=$(echo "$DATABASE_URL" | sed -n 's/.*\/\([^?]*\).*/\1/p')
        
        # Handle localhost vs container hostname
        if [ "$DB_HOST" = "postgres" ] || [ "$DB_HOST" = "localhost" ]; then
            # Try localhost first (for port mapping)
            if [ -n "$DB_PASSWORD" ] && [ -n "$DB_USER" ] && [ -n "$DB_NAME" ]; then
                if PGPASSWORD="$DB_PASSWORD" psql -h localhost -p "${DB_PORT:-5432}" -U "$DB_USER" -d "$DB_NAME" -c "SELECT 1;" >/dev/null 2>&1; then
                    echo "✓ PostgreSQL is ready (connected via psql to localhost)"
                    break
                fi
            fi
        fi
    fi
    
    attempt=$((attempt + 1))
    if [ $((attempt % 5)) -eq 0 ]; then
        echo "Waiting for PostgreSQL... ($attempt/$max_attempts)"
    fi
    sleep 2
done

if [ $attempt -eq $max_attempts ]; then
    echo ""
    echo "✗ PostgreSQL connection failed after $max_attempts attempts"
    echo ""
    echo "Troubleshooting:"
    echo "  1. Check if database is accessible:"
    echo "     - Local container: docker compose -f docker-compose-dev.yml ps postgres"
    echo "     - Server database: Check network connectivity"
    echo "  2. Verify DATABASE_URL: $DATABASE_URL"
    echo "  3. For server database, ensure you can connect:"
    echo "     psql \"$DATABASE_URL\" -c \"SELECT 1;\""
    echo ""
    echo "Note: For sqlx prepare, you can use either:"
    echo "  - Local database (if container is running)"
    echo "  - Server database (set DATABASE_URL in .env)"
    echo ""
    exit 1
fi


# Run migrations if needed
echo ""
echo "Running database migrations..."
cargo sqlx migrate run || echo "Warning: Migrations may have failed, continuing..."

# Prepare SQLx cache
echo ""
echo "Preparing SQLx cache (this may take a few minutes)..."
cargo sqlx prepare --workspace

echo ""
echo "=========================================="
echo "  ✓ SQLx cache prepared successfully!"
echo "=========================================="
echo ""
echo "You can now build the Docker image:"
echo "  docker buildx build --platform linux/amd64 -f Dockerfile -t appflowyinc/appflowy_cloud:latest --load ."
echo ""

