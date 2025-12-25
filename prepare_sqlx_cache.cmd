@echo off
REM Script to prepare SQLx cache before Docker build for Windows
REM This ensures all queries are cached for offline builds
REM
REM Usage:
REM   prepare_sqlx_cache.cmd [database_url]
REM
REM If database_url is not provided, it will try to use DATABASE_URL from .env file
REM or default to postgres://postgres:password@localhost:5432/postgres

setlocal enabledelayedexpansion

echo ==========================================
echo   Preparing SQLx Cache (Windows)
echo ==========================================
echo.

REM Get database URL from command line argument or environment
set "DB_URL=%~1"
if "%DB_URL%"=="" (
    if defined DATABASE_URL (
        set "DB_URL=%DATABASE_URL%"
    )
)

REM Load environment variables from .env file if it exists and DB_URL not set
if "%DB_URL%"=="" (
    if exist .env (
        echo Loading environment variables from .env file...
        for /f "usebackq tokens=1,* delims==" %%a in (.env) do (
            REM Skip empty lines and comments
            if not "%%a"=="" (
                echo %%a | findstr "^#" >nul
                if errorlevel 1 (
                    if "%%a"=="DATABASE_URL" (
                        set "DB_URL=%%b"
                        echo Found DATABASE_URL in .env file
                    )
                )
            )
        )
    )
)

REM Set default DATABASE_URL if not set
if "%DB_URL%"=="" (
    set "DB_URL=postgres://postgres:password@localhost:5432/postgres"
)

REM Export for cargo sqlx
set "DATABASE_URL=%DB_URL%"

echo Using DATABASE_URL: %DATABASE_URL%
echo.

REM Check if database is running (only for local container)
echo Checking database connectivity...

REM Try to connect using cargo sqlx (most reliable method)
echo Testing connection with cargo sqlx...
cargo sqlx database create >nul 2>nul
if %errorlevel% equ 0 (
    echo ✓ Database connection verified via cargo sqlx
    goto :run_migrations
)

REM If cargo sqlx fails, try psql if available
where psql >nul 2>nul
if %errorlevel% equ 0 (
    echo Testing connection with psql...
    psql "%DATABASE_URL%" -c "SELECT 1;" >nul 2>nul
    if %errorlevel% equ 0 (
        echo ✓ Database connection verified via psql
        goto :run_migrations
    )
)

REM If both methods fail, check if we need to start local container
echo Database connection failed. Checking if we need to start local container...
docker compose -f docker-compose-dev.yml ps postgres 2>nul | findstr /R "Up running" >nul
if %errorlevel% neq 0 (
    echo Starting PostgreSQL container...
    docker compose -f docker-compose-dev.yml up -d postgres
    echo Waiting for PostgreSQL to start...
    timeout /t 10 /nobreak >nul

    REM Wait for database to be ready
    echo Waiting for database to be ready...
    set max_attempts=30
    set attempt=0

    :wait_loop
    if %attempt% geq %max_attempts% goto :connection_timeout

    cargo sqlx database create >nul 2>nul
    if %errorlevel% equ 0 (
        echo ✓ Database is ready after %attempt% attempts
        goto :run_migrations
    )

    set /a attempt+=1
    echo Attempt %attempt%/%max_attempts%...
    timeout /t 2 /nobreak >nul
    goto :wait_loop

    :connection_timeout
    echo ✗ Database connection timeout after %max_attempts% attempts
    goto :troubleshooting
)

echo ✗ Database connection failed
goto :troubleshooting

:run_migrations
REM Run migrations if needed
echo.
echo Running database migrations...
cargo sqlx migrate run 2>nul
if %errorlevel% neq 0 (
    echo Warning: Migrations may have failed, continuing...
)

REM Prepare SQLx cache
echo.
echo Preparing SQLx cache (this may take a few minutes)...
cargo sqlx prepare --workspace

if %errorlevel% equ 0 (
    echo.
    echo ==========================================
    echo   ✓ SQLx cache prepared successfully!
    echo ==========================================
    echo.
    echo You can now build the Docker image:
    echo   docker buildx build --platform linux/amd64 -f Dockerfile -t appflowyinc/appflowy_cloud:latest --load .
    echo.
) else (
    echo.
    echo ✗ SQLx cache preparation failed
    echo.
    goto :troubleshooting
)
goto :end

:troubleshooting
echo.
echo Troubleshooting:
echo   1. Check if database is accessible:
echo      - Local container: docker compose -f docker-compose-dev.yml ps postgres
echo      - Server database: Check network connectivity
echo   2. Verify DATABASE_URL: %DATABASE_URL%
echo   3. For server database, ensure you can connect:
echo      psql "%DATABASE_URL%" -c "SELECT 1;"
echo.
echo   4. Manual sqlx preparation:
echo      set DATABASE_URL=%DATABASE_URL%
echo      cargo sqlx prepare --workspace
echo.
echo Note: For sqlx prepare, you can use either:
echo   - Local database (if container is running)
echo   - Server database (pass DATABASE_URL as argument)
echo.
echo Usage: prepare_sqlx_cache.cmd [database_url]
echo.

:end
endlocal
