#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
fixture_dir="$repo_root/deploy/ops/fixtures/orchestrator-docker-agent-e2e"
run_suffix="${GITHUB_RUN_ID:-local}-${GITHUB_RUN_ATTEMPT:-0}-$$"
run_suffix="$(printf '%s' "$run_suffix" | tr '[:upper:]_' '[:lower:]-' | tr -cd 'a-z0-9-' | cut -c1-32)"
registry_name="ojos-agent-e2e-registry-${run_suffix}"
service_id="docker-e2e-${run_suffix}"
registry_username="orchestrator-e2e"
registry_password="orchestrator-e2e-password"

if ! command -v docker >/dev/null 2>&1; then
  echo "Docker CLI is required for the orchestrator Agent production gate" >&2
  exit 1
fi
if ! docker info >/dev/null 2>&1; then
  echo "A reachable Docker Engine is required for the orchestrator Agent production gate" >&2
  exit 1
fi
if [[ ! -f "$fixture_dir/Dockerfile" || ! -f "$fixture_dir/run.sh" ]]; then
  echo "Docker Agent E2E fixture is incomplete: $fixture_dir" >&2
  exit 1
fi

auth_root="$(mktemp -d)"
docker_config="$auth_root/docker-client"
mkdir -p "$docker_config"
printf '%s\n' 'orchestrator-e2e:$2y$05$Wfi68P/Kg9oDv8V28uta.OwXz2/C4Dvhs6jbrjNg29tMYIQ9gzGcm' >"$auth_root/htpasswd"
htpasswd_copy_source="$auth_root/htpasswd"
if command -v cygpath >/dev/null 2>&1; then
  htpasswd_copy_source="$(cygpath -w "$htpasswd_copy_source")"
fi

cleanup() {
  while IFS= read -r container_id; do
    [[ -z "$container_id" ]] || docker rm -f "$container_id" >/dev/null 2>&1 || true
  done < <(docker ps -aq --filter "label=ojos.service_id=$service_id" 2>/dev/null || true)
  docker rm -f "$registry_name" >/dev/null 2>&1 || true
  rm -f -- "$auth_root/htpasswd" "$auth_root/registry-credentials.json" \
    "$docker_config/config.json" 2>/dev/null || true
  rmdir "$docker_config" "$auth_root" 2>/dev/null || true
}
trap cleanup EXIT

registry_publish="127.0.0.1::5000"
if [[ "$(docker info --format '{{.OperatingSystem}}')" == *"Docker Desktop"* ]]; then
  # Docker Desktop runs the daemon in a VM. A host-only bind is not reachable
  # from that daemon's own 127.0.0.0/8 registry path, while an ephemeral all-
  # interface VM bind is. The registry is still short-lived and removed by the
  # EXIT trap; native Linux CI remains loopback-only.
  registry_publish="0:5000"
fi
MSYS_NO_PATHCONV=1 docker create --name "$registry_name" --publish "$registry_publish" \
  --env REGISTRY_AUTH=htpasswd \
  --env REGISTRY_AUTH_HTPASSWD_REALM='OJOS Agent E2E' \
  --env REGISTRY_AUTH_HTPASSWD_PATH=/etc/docker/registry/htpasswd \
  registry:2.8.3 >/dev/null
MSYS_NO_PATHCONV=1 docker cp "$htpasswd_copy_source" \
  "$registry_name:/etc/docker/registry/htpasswd"
docker start "$registry_name" >/dev/null
registry_port="$(docker port "$registry_name" 5000/tcp | awk -F: 'NR == 1 { print $NF }')"
if [[ ! "$registry_port" =~ ^[0-9]+$ ]]; then
  echo "Could not resolve the local registry port" >&2
  exit 1
fi

authenticated_status=""
for _attempt in $(seq 1 60); do
  authenticated_status="$(curl --silent --output /dev/null --write-out '%{http_code}' \
    --user "$registry_username:$registry_password" \
    "http://127.0.0.1:${registry_port}/v2/" || true)"
  if [[ "$authenticated_status" == 200 ]]; then
    break
  fi
  sleep 1
done
if [[ "$authenticated_status" != 200 ]]; then
  docker logs "$registry_name" >&2 || true
  echo "Authenticated registry readiness failed with HTTP $authenticated_status" >&2
  exit 1
fi
anonymous_status="$(curl --silent --output /dev/null --write-out '%{http_code}' \
  "http://127.0.0.1:${registry_port}/v2/")"
if [[ "$anonymous_status" != 401 ]]; then
  echo "The Docker Agent E2E registry must reject anonymous pulls" >&2
  exit 1
fi

printf '%s' "$registry_password" | docker --config "$docker_config" login \
  --username "$registry_username" --password-stdin "127.0.0.1:${registry_port}" >/dev/null
printf '{"schema_version":1,"registries":[{"server_address":"127.0.0.1:%s","username":"%s","password":"%s"}]}\n' \
  "$registry_port" "$registry_username" "$registry_password" \
  >"$auth_root/registry-credentials.json"
chmod 0600 "$auth_root/registry-credentials.json"
registry_credentials_path="$auth_root/registry-credentials.json"
if command -v cygpath >/dev/null 2>&1; then
  registry_credentials_path="$(cygpath -w "$registry_credentials_path")"
fi

# Native Linux Docker Engine marks 127.0.0.0/8 as the local-registry range.
# The registry is also published only on 127.0.0.1, so it never leaves the
# isolated CI runner while still returning a real content RepoDigest.
repository="127.0.0.1:${registry_port}/ojos/orchestrator-agent-e2e"
docker build --build-arg OJOS_E2E_VERSION=1.0.0 --tag "$repository:v1" "$fixture_dir"
docker --config "$docker_config" push "$repository:v1"
docker build --build-arg OJOS_E2E_VERSION=2.0.0 --tag "$repository:v2" "$fixture_dir"
docker --config "$docker_config" push "$repository:v2"

image_v1="$(docker image inspect --format '{{index .RepoDigests 0}}' "$repository:v1")"
image_v2="$(docker image inspect --format '{{index .RepoDigests 0}}' "$repository:v2")"
if [[ ! "$image_v1" =~ ^127\.0\.0\.1:[0-9]+/ojos/orchestrator-agent-e2e@sha256:[0-9a-f]{64}$ ]]; then
  echo "v1 image does not have the expected immutable RepoDigest: $image_v1" >&2
  exit 1
fi
if [[ ! "$image_v2" =~ ^127\.0\.0\.1:[0-9]+/ojos/orchestrator-agent-e2e@sha256:[0-9a-f]{64}$ ]]; then
  echo "v2 image does not have the expected immutable RepoDigest: $image_v2" >&2
  exit 1
fi
if [[ "$image_v1" == "$image_v2" ]]; then
  echo "upgrade fixture must resolve to a different OCI digest" >&2
  exit 1
fi

# Remove the just-built local copies so the Agent must authenticate to the
# Registry during its digest-pinned pull instead of reusing the daemon cache.
docker image rm "$repository:v1" "$repository:v2" >/dev/null
if docker image inspect "$image_v1" >/dev/null 2>&1 || \
  docker image inspect "$image_v2" >/dev/null 2>&1; then
  echo "Docker Agent E2E fixture images must be absent before the authenticated pull" >&2
  exit 1
fi

cd "$repo_root"
OJOS_REQUIRE_DOCKER_AGENT_E2E=1 \
OJOS_DOCKER_E2E_IMAGE_V1="$image_v1" \
OJOS_DOCKER_E2E_IMAGE_V2="$image_v2" \
OJOS_DOCKER_E2E_SERVICE_ID="$service_id" \
OJOS_DOCKER_E2E_REGISTRY_CREDENTIALS="$registry_credentials_path" \
cargo test -p ojos-orchestrator-daemon --test docker_agent_v1_e2e -- --nocapture --test-threads=1
