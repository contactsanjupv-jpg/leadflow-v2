#!/bin/sh
# Apply pending SQL migrations in order, recording each in schema_migrations so the script is
# safe to re-run. Run with a PRIVILEGED role (superuser / BYPASSRLS owner), NOT the runtime
# xflow_app role (spec section 30). Usage: MIGRATION_DATABASE_URL=postgres://... scripts/migrate.sh
#
# Optional: XFLOW_APP_PASSWORD sets the runtime role's password after migrating. Migration 0002
# creates `xflow_app` with a placeholder password, so set this (or run ALTER ROLE yourself) in
# every non-local environment.
set -eu
: "${MIGRATION_DATABASE_URL:?MIGRATION_DATABASE_URL must be set}"
DIR="$(cd "$(dirname "$0")/.." && pwd)/migrations"
PSQL="psql $MIGRATION_DATABASE_URL -v ON_ERROR_STOP=1 -q"

$PSQL -c "CREATE TABLE IF NOT EXISTS schema_migrations (name TEXT PRIMARY KEY, applied_at TIMESTAMPTZ NOT NULL DEFAULT now())"

for f in "$DIR"/*.sql; do
  name="$(basename "$f")"
  done_already="$($PSQL -tA -c "SELECT 1 FROM schema_migrations WHERE name = '$name'")"
  if [ "$done_already" = "1" ]; then
    echo "skip   $name"
    continue
  fi
  echo "apply  $name"
  # Each migration and its bookkeeping row commit atomically.
  { echo "BEGIN;"; cat "$f"; echo "INSERT INTO schema_migrations (name) VALUES ('$name');"; echo "COMMIT;"; } | $PSQL
done

if [ -n "${XFLOW_APP_PASSWORD:-}" ]; then
  echo "ALTER ROLE xflow_app PASSWORD :'pw';" | $PSQL -v pw="$XFLOW_APP_PASSWORD"
  echo "set    xflow_app password"
fi
echo "migrations up to date"
