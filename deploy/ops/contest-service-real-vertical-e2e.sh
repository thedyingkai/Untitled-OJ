#!/usr/bin/env bash
set -euo pipefail

# Required Linux harness for the real contest-service vertical.  This script
# intentionally owns every external dependency and evidence path: callers
# cannot pass a pre-baked success document into the Rust acceptance test.

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
run_id="${GITHUB_RUN_ID:-local}-${GITHUB_RUN_ATTEMPT:-0}-$$"
run_id="$(printf '%s' "$run_id" | tr '[:upper:]_' '[:lower:]-' | tr -cd 'a-z0-9-' | cut -c1-30)"
registry_name="ojos-contest-vertical-registry-${run_id}"
postgres_name="ojos-contest-vertical-postgres-${run_id}"
repository=""
run_parent="/tmp"
run_parent_canonical="$(realpath -e -- "$run_parent" 2>/dev/null || true)"
run_parent_contract="$(stat -c '%u:%g:%a' -- "$run_parent" 2>/dev/null || true)"
if [[ "$run_parent_canonical" != "$run_parent" || ! -d "$run_parent" \
    || -L "$run_parent" || "$run_parent_contract" != "0:0:1777" ]]; then
  echo "contest real vertical requires canonical root-owned sticky /tmp (0:0:1777)" >&2
  exit 1
fi
invoking_uid="$(id -u)"
invoking_gid="$(id -g)"
run_root="$(mktemp -d -p "$run_parent" ojos-contest-real-vertical.XXXXXXXX)"
run_root="$(realpath -e -- "$run_root")"
run_root_validated=0
if [[ ! "$run_root" =~ ^/ || "$run_root" == / || ! -d "$run_root" \
    || -L "$run_root" || "$(dirname -- "$run_root")" != "$run_parent" \
    || ! "$(basename -- "$run_root")" =~ ^ojos-contest-real-vertical\.[A-Za-z0-9]{8}$ \
    || "$(stat -c '%u:%g:%a' -- "$run_root")" != "$invoking_uid:$invoking_gid:700" ]]; then
  echo "mktemp returned an unsafe contest vertical run root: $run_root" >&2
  exit 1
fi
run_root_validated=1
staged_repo_root="$run_root/staged-repo"
scratch_root="$staged_repo_root/.runtime"
output_root="$run_root/output"
evidence="$output_root/live-evidence.json"
gateway_bin="$run_root/ojos-gateway"
staged_contract_source="$staged_repo_root/services/contest-service/ojos.service.yaml"
artifact_pid=""
gateway_tls_pid=""
contest_container_id=""
ca_installed=0
hosts_marker="# ojos-contest-vertical-${run_id}"

# Before privileged workload ownership exists, a runner-owned mktemp root can
# be removed directly. This trap covers fail-closed prerequisite checks; the
# full constrained cleanup below replaces it before any chown occurs.
cleanup_runner_root() {
  if [[ "$run_root_validated" == 1 && -n "$run_root" && "$run_root" != / \
      && "$run_parent" == /tmp \
      && "$(realpath -e -- "$run_parent" 2>/dev/null || true)" == "$run_parent" \
      && "$(stat -c '%u:%g:%a' -- "$run_parent" 2>/dev/null || true)" == "0:0:1777" \
      && "$(dirname -- "$run_root")" == "$run_parent" \
      && "$(basename -- "$run_root")" =~ ^ojos-contest-real-vertical\.[A-Za-z0-9]{8}$ ]]; then
    rm -rf -- "$run_root"
  fi
}
trap cleanup_runner_root EXIT

required_commands=(cargo curl docker go openssl psql python3 redis-cli setpriv socat sudo)
for command_name in "${required_commands[@]}"; do
  if ! command -v "$command_name" >/dev/null 2>&1; then
    echo "contest real vertical requires $command_name" >&2
    exit 1
  fi
done
if ! docker info >/dev/null 2>&1; then
  echo "contest real vertical requires a reachable Docker Engine" >&2
  exit 1
fi
if [[ ! -S /var/run/docker.sock ]]; then
  echo "contest real vertical requires the local Docker Unix socket" >&2
  exit 1
fi

# The signed standard-v3 runtime, its migration, and the Agent-owned private
# bind sources all use the same non-root identity. Build as the normal runner,
# then execute only the dedicated test binary as 65532:65532. The one retained
# supplemental group is the exact Docker socket group required by this
# embedded live Agent; no root/chown capability is available inside the test.
workload_uid=65532
workload_gid=65532
docker_socket_gid="$(stat -c '%g' /var/run/docker.sock)"
if [[ ! "$docker_socket_gid" =~ ^[0-9]+$ || "$docker_socket_gid" == 0 ]]; then
  echo "could not determine Docker socket group" >&2
  exit 1
fi
workload_groups="$docker_socket_gid"

cleanup() {
  [[ -z "$artifact_pid" ]] || kill "$artifact_pid" >/dev/null 2>&1 || true
  [[ -z "$gateway_tls_pid" ]] || kill "$gateway_tls_pid" >/dev/null 2>&1 || true
  while IFS= read -r container_id; do
    [[ -z "$container_id" ]] || docker rm -f "$container_id" >/dev/null 2>&1 || true
  done < <(docker ps -aq --filter "label=ojos.e2e.run=${run_id}" 2>/dev/null || true)
  # The Rust driver writes the exact Agent-observed runtime id before running
  # its assertions. Recover it here as well so a later failed assertion cannot
  # leak the real contest container merely because `cargo test` exited early.
  if [[ -z "$contest_container_id" ]] && sudo test -s "$evidence" 2>/dev/null; then
    contest_container_id="$(sudo python3 - "$evidence" <<'PY' 2>/dev/null || true
import json
import re
import sys

with open(sys.argv[1], "r", encoding="utf-8") as handle:
    value = json.load(handle).get("runtime_container_id", "")
if isinstance(value, str) and re.fullmatch(r"[0-9a-f]{12,64}", value):
    print(value)
PY
)"
  fi
  if [[ -n "$contest_container_id" && "$contest_container_id" =~ ^[0-9a-f]{12,64}$ ]]; then
    docker rm -f "$contest_container_id" >/dev/null 2>&1 || true
  fi
  docker rm -f "$postgres_name" "$registry_name" >/dev/null 2>&1 || true
  if [[ "$ca_installed" == 1 ]]; then
    sudo rm -f "/usr/local/share/ca-certificates/ojos-contest-${run_id}.crt" || true
    sudo update-ca-certificates --fresh >/dev/null 2>&1 || true
  fi
  if grep -Fq "$hosts_marker" /etc/hosts 2>/dev/null; then
    sudo sed -i "\|$hosts_marker|d" /etc/hosts || true
  fi
  # Workload-private paths are owned by 65532 and remain 0700. Re-resolve and
  # constrain the exact mktemp target before privileged cleanup; never widen
  # private modes merely so the invoking runner can remove them.
  if [[ "$run_root_validated" == 1 && -n "$run_root" && "$run_root" != / ]]; then
    cleanup_root="$(realpath -e -- "$run_root" 2>/dev/null || true)"
    if [[ -z "$cleanup_root" ]]; then
      : # The directory was already removed.
    elif [[ "$cleanup_root" == "$run_root" \
        && "$run_parent" == /tmp \
        && "$(realpath -e -- "$run_parent" 2>/dev/null || true)" == "$run_parent" \
        && "$(stat -c '%u:%g:%a' -- "$run_parent" 2>/dev/null || true)" == "0:0:1777" \
        && "$(dirname -- "$cleanup_root")" == "$run_parent" \
        && "$(basename -- "$cleanup_root")" =~ ^ojos-contest-real-vertical\.[A-Za-z0-9]{8}$ ]]; then
      sudo rm -rf -- "$cleanup_root"
    else
      echo "refusing unsafe privileged contest vertical cleanup: $cleanup_root" >&2
    fi
  fi
}
trap cleanup EXIT

# From this point on the privileged cleanup trap is installed. The Agent's
# writable tree is never made group/world accessible. Only the validated
# run-root boundary becomes traversable so numeric uid 65532 can reach its
# explicitly permissioned inputs and private scratch tree through sticky /tmp.
chmod 0755 "$run_root"
if [[ "$(stat -c '%u:%g:%a' -- "$run_root")" != "$invoking_uid:$invoking_gid:755" ]]; then
  echo "contest vertical run root handoff violates the traversal contract" >&2
  exit 1
fi
mkdir -p "$output_root"
# The evidence path is inside this freshly-created, validated mktemp root.
# Remove it while the runner still owns the tree: an arbitrary numeric uid
# cannot be assumed to traverse RUNNER_TEMP's runner-owned ancestors. After
# this point output_root is handed to 65532 without widening any permissions.
if [[ "$(dirname -- "$evidence")" != "$output_root" \
    || "$(basename -- "$evidence")" != "live-evidence.json" ]]; then
  echo "refusing unsafe contest vertical evidence preclean: $evidence" >&2
  exit 1
fi
rm -f -- "$evidence"
sudo chown "$workload_uid:$workload_gid" "$output_root"
sudo chmod 0755 "$output_root"

# The numeric workload identity cannot and must not traverse the runner's
# checkout. Stage only the checked-in inputs required to compile the contest
# Service Contract and initialize the embedded server, preserving their
# relative paths. Everything in this allowlist is non-secret and becomes
# immutable before the privilege drop; no other workspace content is exposed.
staged_inputs=(
  "services/contest-service/ojos.service.yaml"
  "services/contest-service/api/openapi.yaml"
  "services/contest-service/config.schema.json"
  "services/contest-service/events/contest-created-v1.schema.json"
  "services/contest-service/frontend/user/manifest.json"
  "services/contest-service/frontend/admin/manifest.json"
  "platform/schemas/orchestrator/actions.yaml"
  "platform/schemas/orchestrator/forms.yaml"
  "platform/schemas/orchestrator/plans.yaml"
  "platform/schemas/orchestrator/results.yaml"
  "platform/schemas/orchestrator/errors.yaml"
)
mkdir -p "$staged_repo_root"
staged_repo_root="$(realpath -e -- "$staged_repo_root")"
if [[ "$staged_repo_root" != "$run_root/staged-repo" || ! -d "$staged_repo_root" \
    || -L "$staged_repo_root" ]]; then
  echo "contest contract staged root violates the fixed path contract" >&2
  exit 1
fi
for relative_input in "${staged_inputs[@]}"; do
  source_input="$repo_root/$relative_input"
  resolved_source="$(realpath -e -- "$source_input" 2>/dev/null || true)"
  if [[ "$resolved_source" != "$source_input" || ! -f "$resolved_source" \
      || -L "$source_input" ]]; then
    echo "contest contract input violates the checked-in file contract: $relative_input" >&2
    exit 1
  fi
  staged_input="$staged_repo_root/$relative_input"
  install -D -m 0444 -- "$resolved_source" "$staged_input"
done
while IFS= read -r staged_directory; do
  chmod 0555 -- "$staged_directory"
done < <(find "$staged_repo_root" -type d -print)
if [[ "$(find "$staged_repo_root" -type f -print | wc -l)" -ne "${#staged_inputs[@]}" \
    || -n "$(find "$staged_repo_root" -type l -print -quit)" ]]; then
  echo "contest contract staging contains files outside the explicit allowlist" >&2
  exit 1
fi
for relative_input in "${staged_inputs[@]}"; do
  source_input="$repo_root/$relative_input"
  staged_input="$staged_repo_root/$relative_input"
  resolved_staged="$(realpath -e -- "$staged_input" 2>/dev/null || true)"
  if [[ "$resolved_staged" != "$staged_input" || ! -f "$resolved_staged" \
      || -L "$staged_input" \
      || "$(stat -c '%u:%g:%a' -- "$staged_input")" != "$invoking_uid:$invoking_gid:444" \
      || "$resolved_staged" != "$staged_repo_root/"* ]]; then
    echo "staged contest contract input violates the read-only contract: $relative_input" >&2
    exit 1
  fi
  if ! cmp -s -- "$source_input" "$staged_input"; then
    echo "staged contest contract input differs from the checkout: $relative_input" >&2
    exit 1
  fi
done
if [[ "$(stat -c '%u:%g:%a' -- "$staged_repo_root")" != "$invoking_uid:$invoking_gid:555" \
    || "$(realpath -e -- "$staged_contract_source")" != "$staged_contract_source" ]]; then
  echo "contest contract staged root or entrypoint violates the final contract" >&2
  exit 1
fi
# Catalog fixtures must remain beneath the embedded server's repository root,
# so reserve one dedicated writable subtree. Only uid 65532 can enter it; the
# staged checkout inputs above remain runner-owned and read-only.
sudo install -d -o "$workload_uid" -g "$workload_gid" -m 0700 -- \
  "$scratch_root" "$scratch_root/home" "$scratch_root/tmp"
scratch_root="$(sudo realpath -e -- "$scratch_root")"
if [[ "$scratch_root" != "$staged_repo_root/.runtime" \
    || "$(sudo stat -c '%u:%g:%a' -- "$scratch_root")" != "$workload_uid:$workload_gid:700" \
    || "$(sudo stat -c '%u:%g:%a' -- "$scratch_root/home")" != "$workload_uid:$workload_gid:700" \
    || "$(sudo stat -c '%u:%g:%a' -- "$scratch_root/tmp")" != "$workload_uid:$workload_gid:700" ]]; then
  echo "contest workload subtree violates the private ownership contract" >&2
  exit 1
fi

free_port() {
  python3 - <<'PY'
import socket
with socket.socket() as sock:
    sock.bind(("127.0.0.1", 0))
    print(sock.getsockname()[1])
PY
}

wait_http() {
  local url="$1"
  local expected="${2:-200}"
  local status=""
  for _attempt in $(seq 1 90); do
    status="$(curl --silent --insecure --output /dev/null --write-out '%{http_code}' "$url" || true)"
    [[ "$status" == "$expected" ]] && return 0
    sleep 1
  done
  echo "readiness failed for $url (last HTTP $status)" >&2
  return 1
}

registry_port="$(free_port)"
postgres_port="$(free_port)"
artifact_port="$(free_port)"
gateway_port="$(free_port)"
gateway_tls_port="$(free_port)"
bridge_gateway="$(docker network inspect bridge --format '{{(index .IPAM.Config 0).Gateway}}')"
if [[ ! "$bridge_gateway" =~ ^[0-9a-fA-F:.]+$ ]]; then
  echo "could not resolve the Docker bridge gateway" >&2
  exit 1
fi

docker run --detach --name "$registry_name" \
  --label "ojos.e2e.run=${run_id}" \
  --publish "127.0.0.1:${registry_port}:5000" registry:2.8.3 >/dev/null
wait_http "http://127.0.0.1:${registry_port}/v2/"
repository="127.0.0.1:${registry_port}/ojos/contest-service"

docker build --file "$repo_root/services/contest-service/Dockerfile" \
  --tag "$repository:runtime" "$repo_root"
docker push "$repository:runtime"
runtime_image="$(docker image inspect --format '{{index .RepoDigests 0}}' "$repository:runtime")"
docker build --file "$repo_root/services/contest-service/migrations/Dockerfile" \
  --tag "$repository:migration" "$repo_root"
docker push "$repository:migration"
migration_image="$(docker image inspect --format '{{index .RepoDigests 0}}' "$repository:migration")"
for image in "$runtime_image" "$migration_image"; do
  if [[ ! "$image" =~ @sha256:[0-9a-f]{64}$ ]]; then
    echo "contest OCI is not digest-pinned: $image" >&2
    exit 1
  fi
done

(cd "$repo_root/services/gateway" && go build -trimpath -buildvcs=false -o "$gateway_bin" .)
[[ -x "$gateway_bin" ]] || { echo "real Gateway binary was not built" >&2; exit 1; }

mkdir -p "$run_root/tls" "$run_root/artifacts"
openssl req -x509 -newkey rsa:2048 -nodes -days 1 \
  -subj '/CN=OJOS contest vertical CA' \
  -keyout "$run_root/tls/ca.key" -out "$run_root/tls/ca.crt" >/dev/null 2>&1
openssl req -newkey rsa:2048 -nodes -subj '/CN=gateway.test' \
  -keyout "$run_root/tls/server.key" -out "$run_root/tls/server.csr" >/dev/null 2>&1
cat >"$run_root/tls/server.ext" <<EOF
subjectAltName=DNS:gateway.test,DNS:artifacts.test,IP:127.0.0.1,IP:${bridge_gateway}
extendedKeyUsage=serverAuth
EOF
openssl x509 -req -days 1 -sha256 \
  -in "$run_root/tls/server.csr" -CA "$run_root/tls/ca.crt" \
  -CAkey "$run_root/tls/ca.key" -CAcreateserial \
  -extfile "$run_root/tls/server.ext" -out "$run_root/tls/server.crt" >/dev/null 2>&1
chmod 0755 "$run_root/tls" "$run_root/artifacts"
chmod 0644 "$run_root/tls/ca.crt" "$run_root/tls/server.crt"
chmod 0600 "$run_root/tls/server.key" "$run_root/tls/ca.key"
sudo install -m 0644 "$run_root/tls/ca.crt" \
  "/usr/local/share/ca-certificates/ojos-contest-${run_id}.crt"
sudo update-ca-certificates >/dev/null
ca_installed=1
printf '127.0.0.1 gateway.test artifacts.test %s\n' "$hosts_marker" | sudo tee -a /etc/hosts >/dev/null

postgres_password="ojos-contest-live-${run_id}-password"
docker run --detach --name "$postgres_name" \
  --label "ojos.e2e.run=${run_id}" \
  --publish "0.0.0.0:${postgres_port}:5432" \
  --env POSTGRES_PASSWORD="$postgres_password" \
  --volume "$run_root/tls:/ojos-tls:ro" \
  postgres:17.6-bookworm bash -ec '
    cp /ojos-tls/server.crt /var/lib/postgresql/server.crt
    cp /ojos-tls/server.key /var/lib/postgresql/server.key
    chown postgres:postgres /var/lib/postgresql/server.crt /var/lib/postgresql/server.key
    chmod 0600 /var/lib/postgresql/server.key
    exec docker-entrypoint.sh postgres -c ssl=on \
      -c ssl_cert_file=/var/lib/postgresql/server.crt \
      -c ssl_key_file=/var/lib/postgresql/server.key
  ' >/dev/null
for _attempt in $(seq 1 90); do
  if PGPASSWORD="$postgres_password" psql \
      "host=127.0.0.1 port=$postgres_port dbname=postgres user=postgres sslmode=verify-full sslrootcert=$run_root/tls/ca.crt" \
      -XAt -c 'SELECT 1' 2>/dev/null | grep -qx 1; then
    break
  fi
  sleep 1
done
PGPASSWORD="$postgres_password" psql \
  "host=127.0.0.1 port=$postgres_port dbname=postgres user=postgres sslmode=verify-full sslrootcert=$run_root/tls/ca.crt" \
  -XAt -c 'SELECT 1' | grep -qx 1

cp "$repo_root/services/contest-service/frontend/user/bundle.js" "$run_root/artifacts/contest-user.js"
cp "$repo_root/services/contest-service/frontend/admin/bundle.js" "$run_root/artifacts/contest-admin.js"
sudo chown -R "$workload_uid:$workload_gid" "$run_root/artifacts"
sudo chmod 0755 "$run_root/artifacts"
sudo find "$run_root/artifacts" -type d -exec chmod 0755 {} +
sudo find "$run_root/artifacts" -type f -exec chmod 0644 {} +
python3 "$repo_root/deploy/ops/fixtures/contest-service-real-vertical/https_artifact_server.py" \
  --root "$run_root/artifacts" --port "$artifact_port" \
  --cert "$run_root/tls/server.crt" --key "$run_root/tls/server.key" &
artifact_pid=$!
wait_http "https://artifacts.test:${artifact_port}/contest-user.js"

# The real Gateway is started by the Rust driver after its embedded
# Orchestrator has selected a port. This TLS listener exists first so the
# Agent-written service context can point at a stable container-reachable URL.
socat "OPENSSL-LISTEN:${gateway_tls_port},bind=0.0.0.0,reuseaddr,fork,cert=${run_root}/tls/server.crt,key=${run_root}/tls/server.key,cafile=${run_root}/tls/ca.crt,verify=0" \
  "TCP4:127.0.0.1:${gateway_port}" &
gateway_tls_pid=$!

cd "$repo_root"
cargo_build_json="$run_root/contest-test-build.jsonl"
cargo_target_root="$(cargo metadata --no-deps --format-version=1 | python3 -c '
import json
import os
import sys

value = json.load(sys.stdin).get("target_directory")
if not isinstance(value, str) or not os.path.isabs(value):
    raise SystemExit("cargo metadata omitted an absolute target_directory")
print(os.path.realpath(value))
')"
cargo_target_root="$(realpath -e -- "$cargo_target_root")"
cargo test -p ojos-orchestrator-daemon --test contest_service_real_vertical_e2e \
  --no-run --message-format=json-render-diagnostics >"$cargo_build_json"
test_binary="$(python3 - "$cargo_build_json" "$cargo_target_root" "$repo_root" <<'PY'
import json
import os
import re
import sys

executables = []
target_root = os.path.realpath(sys.argv[2])
expected_source = os.path.realpath(
    os.path.join(
        sys.argv[3],
        "services/orchestrator/backend/tests/contest_service_real_vertical_e2e.rs",
    )
)
with open(sys.argv[1], "r", encoding="utf-8") as handle:
    for line in handle:
        try:
            value = json.loads(line)
        except json.JSONDecodeError:
            continue
        target = value.get("target", {})
        executable = value.get("executable")
        if (
            value.get("reason") == "compiler-artifact"
            and target.get("name") == "contest_service_real_vertical_e2e"
            and "test" in target.get("kind", [])
            and os.path.realpath(target.get("src_path", "")) == expected_source
            and isinstance(executable, str)
        ):
            resolved = os.path.realpath(executable)
            if os.path.commonpath((target_root, resolved)) != target_root:
                raise SystemExit(f"contest test executable escaped Cargo target: {resolved!r}")
            if not re.fullmatch(
                r"contest_service_real_vertical_e2e-[0-9a-f]{16,64}",
                os.path.basename(resolved),
            ):
                raise SystemExit(f"unexpected contest test executable name: {resolved!r}")
            executables.append(resolved)
if len(set(executables)) != 1:
    raise SystemExit(f"expected one dedicated contest test binary, got {executables!r}")
print(executables[0])
PY
)"
if [[ ! -x "$test_binary" ]]; then
  echo "dedicated contest vertical test binary is not executable: $test_binary" >&2
  exit 1
fi
staged_test_binary="$run_root/contest-service-real-vertical-e2e"
install -m 0555 -- "$test_binary" "$staged_test_binary"
staged_test_binary="$(realpath -e -- "$staged_test_binary")"
if [[ "$staged_test_binary" != "$run_root/contest-service-real-vertical-e2e" \
    || ! -f "$staged_test_binary" || -L "$staged_test_binary" \
    || "$(stat -c '%a' "$staged_test_binary")" != 555 \
    || ! -x "$staged_test_binary" ]]; then
  echo "staged contest vertical test binary violates the fixed execution contract" >&2
  exit 1
fi
if ! cmp -s -- "$test_binary" "$staged_test_binary"; then
  echo "staged contest vertical test binary differs from the Cargo artifact" >&2
  exit 1
fi

sudo env -i \
  "HOME=$scratch_root/home" \
  "PATH=$PATH" \
  "TMPDIR=$scratch_root/tmp" \
  "RUST_BACKTRACE=1" \
  "OJOS_REQUIRE_CONTEST_REAL_VERTICAL_E2E=1" \
  "OJOS_CONTEST_E2E_DRIVER_OUTPUT=$evidence" \
  "OJOS_CONTEST_E2E_RUNTIME_IMAGE=$runtime_image" \
  "OJOS_CONTEST_E2E_MIGRATION_IMAGE=$migration_image" \
  "OJOS_CONTEST_E2E_GATEWAY_BIN=$gateway_bin" \
  "OJOS_CONTEST_E2E_GATEWAY_HTTP_PORT=$gateway_port" \
  "OJOS_CONTEST_E2E_GATEWAY_ORIGIN=https://gateway.test:${gateway_tls_port}" \
  "OJOS_CONTEST_E2E_GATEWAY_CONTAINER_ORIGIN=https://${bridge_gateway}:${gateway_tls_port}" \
  "OJOS_CONTEST_E2E_ARTIFACT_ORIGIN=https://artifacts.test:${artifact_port}" \
  "OJOS_CONTEST_E2E_ARTIFACT_ROOT=$run_root/artifacts" \
  "OJOS_CONTEST_E2E_STAGED_REPO_ROOT=$staged_repo_root" \
  "OJOS_CONTEST_E2E_CONTRACT_SOURCE=$staged_contract_source" \
  "OJOS_CONTEST_E2E_POSTGRES_PROVIDER_HOST=$bridge_gateway" \
  "OJOS_CONTEST_E2E_POSTGRES_PROVIDER_PORT=$postgres_port" \
  "OJOS_CONTEST_E2E_POSTGRES_ADMIN_URL=postgresql://postgres:${postgres_password}@${bridge_gateway}:${postgres_port}/postgres?sslmode=require" \
  "OJOS_CONTEST_E2E_POSTGRES_CA_FILE=$run_root/tls/ca.crt" \
  "OJOS_CONTEST_E2E_REDIS_URL=redis://${bridge_gateway}:6379/0" \
  "OJOS_CONTEST_E2E_EVIDENCE=$evidence" \
  "OJOS_CONTEST_E2E_SCRATCH_ROOT=$scratch_root" \
  "$(command -v setpriv)" \
    --reuid "$workload_uid" --regid "$workload_gid" \
    --groups "$workload_groups" \
    --bounding-set=-all --inh-caps=-all --ambient-caps=-all \
    --no-new-privs \
    /bin/bash -c 'umask 0022; exec "$@"' contest-vertical \
      "$staged_test_binary" --nocapture --test-threads=1

if [[ ! -s "$evidence" ]]; then
  echo "real vertical driver did not create evidence: $evidence" >&2
  exit 1
fi
if [[ "$(stat -c '%u:%g:%a' "$evidence")" != "$workload_uid:$workload_gid:644" ]]; then
  echo "live evidence owner/mode does not match the non-secret workload output contract" >&2
  exit 1
fi
contest_container_id="$(python3 - "$evidence" <<'PY'
import json
import re
import sys

with open(sys.argv[1], "r", encoding="utf-8") as handle:
    value = json.load(handle).get("runtime_container_id", "")
if not isinstance(value, str) or not re.fullmatch(r"[0-9a-f]{12,64}", value):
    raise SystemExit("live evidence omitted a valid Docker runtime container id")
print(value)
PY
)"
