#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$script_dir/../.." && pwd)"
bash_bin="${BASH:-bash}"

die() {
  echo "preflight: $*" >&2
  exit 1
}

need_cmd() {
  command -v "$1" >/dev/null 2>&1 || die "$1 is required"
}

need_cmd docker
need_cmd "$bash_bin"

monitoring_file="${OJOS_MONITORING_COMPOSE_FILE:-$repo_root/deploy/ops/monitoring/docker-compose.yml}"
skip_monitoring_checks="$(printf '%s' "${OJOS_SKIP_MONITORING_CHECKS:-0}" | tr '[:upper:]' '[:lower:]')"
case "$skip_monitoring_checks" in
  1|true|yes|on) skip_monitoring_checks=1 ;;
  0|false|no|off) skip_monitoring_checks=0 ;;
  *) die "OJOS_SKIP_MONITORING_CHECKS must be a boolean" ;;
esac
if [[ "$skip_monitoring_checks" != "1" ]]; then
  [[ -f "$monitoring_file" ]] || die "monitoring compose file does not exist: $monitoring_file"
  export OJOS_SECRET_CHECK_REQUIRE_ALERTS="${OJOS_SECRET_CHECK_REQUIRE_ALERTS:-1}"
  export OJOS_SECRET_CHECK_REQUIRE_MONITORING="${OJOS_SECRET_CHECK_REQUIRE_MONITORING:-1}"
fi

"$bash_bin" "$script_dir/secret-check.sh"

compose_file="${OJOS_COMPOSE_FILE:-$repo_root/deploy/compose/docker-compose.yml}"
[[ -f "$compose_file" ]] || die "compose file does not exist: $compose_file"

compose_args=(-f "$compose_file")
if [[ -n "${OJOS_ENV_FILE:-}" ]]; then
  compose_args=(--env-file "$OJOS_ENV_FILE" "${compose_args[@]}")
fi

rendered="$(mktemp)"
trap 'rm -f "$rendered"' EXIT
docker compose "${compose_args[@]}" config >"$rendered"

if grep -Eq '^[[:space:]]+judge-worker:' "$rendered" || grep -q 'OJOS_RUNNER_MODE:' "$rendered"; then
  die "production Compose must not enable the legacy-development Judge Worker profile"
fi
if grep -Eq 'ojos-local|static-compose|local-jwt|local-worker|local-internal|minio-local|<[^>]+>' "$rendered"; then
  die "rendered compose still contains local/default placeholders"
fi
if grep -Eq 'minio/minio:latest|redis:8([[:space:]]|$)' "$rendered"; then
  die "rendered compose contains floating runtime image tags"
fi

docker compose "${compose_args[@]}" config --quiet

if [[ "$skip_monitoring_checks" != "1" ]]; then
  monitoring_args=(-f "$monitoring_file")
  if [[ -n "${OJOS_ENV_FILE:-}" ]]; then
    monitoring_args=(--env-file "$OJOS_ENV_FILE" "${monitoring_args[@]}")
  fi
  docker compose "${monitoring_args[@]}" config --quiet
fi

echo "preflight: production deployment policy passed"
