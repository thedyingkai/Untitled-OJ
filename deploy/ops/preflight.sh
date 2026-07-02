#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$script_dir/../.." && pwd)"

die() {
  echo "preflight: $*" >&2
  exit 1
}

need_cmd() {
  command -v "$1" >/dev/null 2>&1 || die "$1 is required"
}

need_cmd docker

monitoring_file="${OJOS_MONITORING_COMPOSE_FILE:-$repo_root/deploy/ops/monitoring/docker-compose.yml}"
if [[ -f "$monitoring_file" ]]; then
  export OJOS_SECRET_CHECK_REQUIRE_ALERTS="${OJOS_SECRET_CHECK_REQUIRE_ALERTS:-1}"
  export OJOS_SECRET_CHECK_REQUIRE_MONITORING="${OJOS_SECRET_CHECK_REQUIRE_MONITORING:-1}"
fi

bash "$script_dir/secret-check.sh"

compose_file="${OJOS_COMPOSE_FILE:-$repo_root/deploy/compose/docker-compose.yml}"
[[ -f "$compose_file" ]] || die "compose file does not exist: $compose_file"

compose_args=(-f "$compose_file")
if [[ -n "${OJOS_ENV_FILE:-}" ]]; then
  compose_args=(--env-file "$OJOS_ENV_FILE" "${compose_args[@]}")
fi

rendered="$(mktemp)"
docker compose "${compose_args[@]}" config >"$rendered"

grep -q 'OJOS_RUNNER_MODE: nsjail' "$rendered" || die "judge-worker must render with OJOS_RUNNER_MODE=nsjail"
runner_lines="$(grep 'OJOS_RUNNER_MODE:' "$rendered" || true)"
if [[ -n "$runner_lines" ]] && grep -v 'OJOS_RUNNER_MODE: nsjail' <<<"$runner_lines" >/dev/null; then
  die "unsupported judge-worker runner mode rendered in production compose"
fi
if grep -q 'OJOS_ALLOW_CGROUP_FALLBACK: "true"' "$rendered" || grep -q 'OJOS_ALLOW_CGROUP_FALLBACK: true' "$rendered"; then
  die "judge-worker must not render with cgroup fallback enabled"
fi
if grep -Eq 'ojos-local|static-compose|local-jwt|local-worker|local-internal|minio-local|<[^>]+>' "$rendered"; then
  die "rendered compose still contains local/default placeholders"
fi
if grep -Eq 'minio/minio:latest|redis:8([[:space:]]|$)' "$rendered"; then
  die "rendered compose contains floating runtime image tags"
fi

docker compose "${compose_args[@]}" config --quiet

if [[ -f "$monitoring_file" ]]; then
  monitoring_args=(-f "$monitoring_file")
  if [[ -n "${OJOS_ENV_FILE:-}" ]]; then
    monitoring_args=(--env-file "$OJOS_ENV_FILE" "${monitoring_args[@]}")
  fi
  docker compose "${monitoring_args[@]}" config --quiet
fi

echo "preflight: production deployment policy passed"
