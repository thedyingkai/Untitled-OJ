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

migration_version() {
  basename "$1" | sed -E 's/^0*([0-9]+)_.*/\1/'
}

max_migration_version() {
  max=0
  for file in "$MIGRATIONS_DIR"/*.up.sql; do
    version="$(migration_version "$file")"
    if [ "$version" -gt "$max" ]; then
      max="$version"
    fi
  done
  echo "$max"
}

ensure_schema_migrations() {
  table_exists="$(psql_base -tA <<'SQL'
SELECT CASE WHEN to_regclass('public.schema_migrations') IS NULL THEN 'no' ELSE 'yes' END;
SQL
)"

  if [ "$table_exists" = "no" ]; then
    echo "creating schema_migrations"
    psql_base <<'SQL'
CREATE TABLE schema_migrations (
  version BIGINT NOT NULL PRIMARY KEY,
  dirty BOOLEAN NOT NULL DEFAULT FALSE
);
SQL
    return
  fi

  version_type="$(psql_base -tA <<'SQL'
SELECT data_type
FROM information_schema.columns
WHERE table_schema = 'public'
  AND table_name = 'schema_migrations'
  AND column_name = 'version';
SQL
)"

  if [ "$version_type" != "bigint" ]; then
    legacy_version="$(psql_base -tA <<'SQL'
SELECT COALESCE(MAX((substring(version::text from '^0*([0-9]+)'))::bigint), 0)
FROM schema_migrations
WHERE substring(version::text from '^0*([0-9]+)') IS NOT NULL;
SQL
)"
    echo "converting legacy schema_migrations to bigint; detected version ${legacy_version}"
    psql_base -v version="$legacy_version" <<'SQL'
DROP TABLE schema_migrations;
CREATE TABLE schema_migrations (
  version BIGINT NOT NULL PRIMARY KEY,
  dirty BOOLEAN NOT NULL DEFAULT FALSE
);
INSERT INTO schema_migrations(version, dirty)
SELECT :'version'::bigint, FALSE
WHERE :'version'::bigint > 0;
SQL
    return
  fi

  dirty_exists="$(psql_base -tA <<'SQL'
SELECT CASE WHEN EXISTS (
  SELECT 1
  FROM information_schema.columns
  WHERE table_schema = 'public'
    AND table_name = 'schema_migrations'
    AND column_name = 'dirty'
) THEN 'yes' ELSE 'no' END;
SQL
)"

  if [ "$dirty_exists" = "no" ]; then
    echo "adding dirty column to schema_migrations"
    psql_base <<'SQL'
ALTER TABLE schema_migrations
  ADD COLUMN dirty BOOLEAN NOT NULL DEFAULT FALSE;
SQL
  fi
}

set_migration_state() {
  version="$1"
  dirty="$2"
  psql_base -v version="$version" -v dirty="$dirty" <<'SQL'
DELETE FROM schema_migrations;
INSERT INTO schema_migrations(version, dirty)
VALUES (:'version'::bigint, :'dirty'::boolean);
SQL
}

ensure_schema_migrations

dirty_state="$(psql_base -tA <<'SQL'
SELECT COALESCE((SELECT dirty::text FROM schema_migrations ORDER BY version DESC LIMIT 1), 'false');
SQL
)"

if [ "$dirty_state" = "true" ]; then
  echo "schema_migrations is dirty; refusing to continue" >&2
  exit 1
fi

schema_ready="$(psql_base -tA <<'SQL'
SELECT CASE
  WHEN to_regclass('public.service_sets') IS NOT NULL
   AND to_regclass('public.judge_tasks') IS NOT NULL
  THEN 'yes'
  ELSE 'no'
END;
SQL
)"

current_version="$(psql_base -tA <<'SQL'
SELECT COALESCE(MAX(version), 0)
FROM schema_migrations
WHERE dirty = FALSE;
SQL
)"

max_version="$(max_migration_version)"

if [ "$schema_ready" = "yes" ] && [ "$current_version" = "0" ]; then
  echo "schema already exists; seeding migration history"
  set_migration_state "$max_version" false
  exit 0
fi

for file in "$MIGRATIONS_DIR"/*.up.sql; do
  version="$(migration_version "$file")"

  if [ "$version" -le "$current_version" ]; then
    echo "skip $version"
    continue
  fi

  echo "apply $version"
  set_migration_state "$version" true
  psql_base -f "$file"
  set_migration_state "$version" false
  current_version="$version"
done

echo "migrations complete"
