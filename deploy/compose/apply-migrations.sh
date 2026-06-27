#!/bin/sh
set -eu

POSTGRES_HOST="${POSTGRES_HOST:-postgres}"
POSTGRES_PORT="${POSTGRES_PORT:-5432}"
POSTGRES_USER="${POSTGRES_USER:-postgres}"
POSTGRES_DB="${POSTGRES_DB:-ojos}"
MIGRATIONS_DIR="${MIGRATIONS_DIR:-/migrations}"

export PGPASSWORD="${POSTGRES_PASSWORD:?POSTGRES_PASSWORD is required}"

psql_base() {
  psql \
    -h "$POSTGRES_HOST" \
    -p "$POSTGRES_PORT" \
    -U "$POSTGRES_USER" \
    -d "$POSTGRES_DB" \
    -v ON_ERROR_STOP=1 \
    "$@"
}

echo "creating schema_migrations if needed"
psql_base <<'SQL'
CREATE TABLE IF NOT EXISTS schema_migrations (
  version TEXT PRIMARY KEY,
  applied_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
SQL

schema_ready="$(psql_base -tA <<'SQL'
SELECT CASE
  WHEN to_regclass('public.module_sets') IS NOT NULL
   AND to_regclass('public.judge_tasks') IS NOT NULL
  THEN 'yes'
  ELSE 'no'
END;
SQL
)"

history_count="$(psql_base -tA <<'SQL'
SELECT COUNT(*) FROM schema_migrations;
SQL
)"

if [ "$schema_ready" = "yes" ] && [ "$history_count" = "0" ]; then
  echo "schema already exists; seeding migration history"
  for file in "$MIGRATIONS_DIR"/*.up.sql; do
    version="$(basename "$file")"
    psql_base -v version="$version" <<'SQL'
INSERT INTO schema_migrations(version)
VALUES (:'version')
ON CONFLICT(version) DO NOTHING;
SQL
  done
  exit 0
fi

for file in "$MIGRATIONS_DIR"/*.up.sql; do
  version="$(basename "$file")"
  applied="$(psql_base -tA -v version="$version" <<'SQL'
SELECT CASE
  WHEN EXISTS (SELECT 1 FROM schema_migrations WHERE version = :'version')
  THEN 'yes'
  ELSE 'no'
END;
SQL
)"

  if [ "$applied" = "yes" ]; then
    echo "skip $version"
    continue
  fi

  echo "apply $version"
  psql_base -f "$file"
  psql_base -v version="$version" <<'SQL'
INSERT INTO schema_migrations(version)
VALUES (:'version')
ON CONFLICT(version) DO NOTHING;
SQL
done

echo "migrations complete"
