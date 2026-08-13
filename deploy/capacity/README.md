# Orchestrator v1 production-capacity environment

This directory builds the dedicated environment consumed by
`orchestrator-capacity.yml`. It is intentionally provider-neutral: inventory
contains ordinary Linux hosts and the playbook uses SSH, Docker Compose and
public Orchestrator v1 APIs. It does not call a cloud control plane and never
writes the Orchestrator database directly.

## Fixed production shape

- one PostgreSQL host with TLS enabled;
- one single-active control-plane host;
- one dedicated GitHub Actions soak runner that carries
  `self-hosted,linux,x64,orchestrator-soak`;
- exactly ten worker hosts;
- ten privileged Docker-in-Docker daemons on every worker, each with its own
  data volume and Unix socket;
- one non-root Agent, mTLS identity directory and SQLite ledger for each
  Engine; every Node advertises the Store contract labels
  `runtime=docker`, `os=linux` and `arch=x86_64`.

The runner only drives the API and records evidence. It does not host the
control plane, PostgreSQL, an Agent, a fixture container or a Docker Engine
that executes business workloads.

Each worker keeps two disjoint per-ordinal host roots. `agent-internal` contains
the mTLS identity, execution/provider SQLite ledgers and generated provider
credentials; it is mounted only into the Agent. `workload-exports` contains
runtime contexts and ResourceClaim outputs; it is mounted read-write into the
matching Agent and read-only at the same absolute path in the matching DinD
daemon. Evidence rejects any Engine mount of the internal root, Registry
credential, provider descriptor/admin URL, or transport CA. An older combined
`agents/<ordinal>` tree stops provisioning after the Agents are stopped; the
playbook never copies or classifies legacy credentials automatically.

## Protected inputs

Copy `inventory.example.yml` and `group_vars/all.example.yml` outside the
repository or encrypt their real replacements with Ansible Vault. Do not
commit filled values. The playbook fails before deployment unless every image
is `repository@sha256:<64 lowercase hex>` and the candidate is a real 40-byte
commit identity.

The PostgreSQL bundle must contain `server.crt`, `server.key`, `root.crt` and
`postgres-password`. Capacity also requires three distinct controller-protected
Agent ResourceClaim inputs: a strict JSON descriptor, a single-line PostgreSQL
administrator URL, and the CA used by its `verify-full` provider. The installed
descriptor is schema v1 and must contain exactly:

```json
{
  "schema_version": 1,
  "provider_id": "postgresql-capacity",
  "host": "postgres.capacity.internal",
  "port": 5432,
  "tls_mode": "verify-full",
  "admin_url_file": "/run/agent-resource-provider/admin.url",
  "ca_file": "/run/agent-resource-provider/postgres-ca.crt"
}
```

The administrator URL stays in Agent-only configuration and uses
`sslmode=require`; the provider descriptor plus the dedicated CA enforce
hostname verification. Generated per-database credentials stay under
`agent-internal`; only generated workload DSN outputs are placed in
`workload-exports` with mode `0600`.

The control-plane bundle must contain:

- `orchestrator-postgres-ca.crt`;
- `orchestrator-tls.crt` and `orchestrator-tls.key`;
- `orchestrator-health-ca.crt`;
- `orchestrator-node-ca.crt` and `orchestrator-node-ca.key`;
- `catalog-tls.crt` and `catalog-tls.key`.

The protected control-plane env file supplies the PostgreSQL URLs with
`sslmode=verify-full`, OIDC configuration, Gateway/Auth management providers,
internal compatibility credential and all other production preflight values.
Capacity deliberately uses externally operated platform providers instead of
starting Auth/Gateway on the candidate control-plane host. The three origins
`ORCHESTRATOR_GATEWAY_ADMIN_ORIGIN`, `ORCHESTRATOR_AUTH_ADMIN_ORIGIN`, and
`ORCHESTRATOR_AUTH_WORKLOAD_ORIGIN` must therefore be credential-free HTTPS
origins. Their three raw credentials must be distinct, header-safe values of at
least 32 bytes. Before any Node code is issued, Ansible performs authenticated
read-only projection probes against Gateway and Auth and mints one disposable
workload credential through Auth; a missing, unreachable, misidentified, or
unauthorized provider stops provisioning.
It also contains only the canonical SHA-256 verifiers for the dedicated
Gateway and Auth Contribution acknowledgement tokens. The matching raw tokens
are distributed separately to those managed services and never copied to the
control-plane host.
Catalog trust and source JSON are generated separately and override only the
two Catalog variables.

The Catalog signing-key file is canonical padded base64 for exactly 32 raw
Ed25519 seed bytes. The key remains on the Ansible controller. The generated
Catalog uses RFC 8785/JCS signing, SHA-256 metadata packages and one immutable
OCI digest for 20 independent service identities. Only the public key and
signature are copied to the control-plane host.

Fixture publication is crash-safe. The generator writes a unique sibling
`pending` tree with `create_new`, syncs every file and directory, verifies the
complete signed Catalog/metadata tree, then renames it atomically to the formal
directory. A later run quarantines stale pending or incomplete formal trees.
An already verified formal tree is accepted only when every byte matches the
same candidate inputs; a different signed tree is never overwritten.

`capacity_image_provenance_record_file` is not a user-authored SHA file. It is
the exact combined JSON record downloaded from the successful
`orchestrator-candidate-images.yml` run after independently verifying all three
OCI subjects and their GitHub attestations. The playbook requires the record to
bind the control-plane, Agent and capacity-fixture digests to the same candidate
commit. It also pulls each digest and reads
`org.opencontainers.image.revision` from the actual image configuration before
starting any candidate component.

## Candidate-image bootstrap

Production provisioning accepts only the three artifacts produced by the
successful first attempt of `orchestrator-candidate-images.yml`; locally built
or manually pushed images cannot substitute for that run. Before rendering the
protected Ansible variables, download and independently verify the exact run:

```bash
set -Eeuo pipefail

export GITHUB_REPOSITORY=OWNER/REPOSITORY
export CANDIDATE_SHA="$(git rev-parse HEAD)"
export CANDIDATE_IMAGE_RUN_ID=REPLACE_WITH_VERIFIED_RUN_ID
export capacity_registry_server=ghcr.io
export capacity_registry_username=REPLACE_WITH_READ_ONLY_PACKAGE_USER
export capacity_candidate_output_dir=/protected/orchestrator-capacity
export capacity_registry_password_file=/protected/orchestrator-capacity/ghcr-read-token
export capacity_runner_gate_env_base_file=/protected/orchestrator-capacity/runner-base.env
export capacity_image_provenance_record_file=/protected/orchestrator-capacity/candidate-image-provenance.json
export capacity_candidate_vars_file=/protected/orchestrator-capacity/candidate-images.vars.json
export capacity_runner_candidate_bindings_file=/protected/orchestrator-capacity/candidate-images.runner-bindings.env
export capacity_runner_gate_env_file=/protected/orchestrator-capacity/runner.env

install -d -m 0700 "$capacity_candidate_output_dir"
test ! -L "$capacity_candidate_output_dir"
test -r "$capacity_registry_password_file"
test -r "$capacity_runner_gate_env_base_file"
work="$(mktemp -d)"
persist_stage=""
docker_config_was_set=0
previous_docker_config=""
if [[ -v DOCKER_CONFIG ]]; then
  docker_config_was_set=1
  previous_docker_config="$DOCKER_CONFIG"
fi
cleanup_candidate_bootstrap() {
  if [[ -n "${persist_stage:-}" ]]; then
    rm -rf -- "$persist_stage" || true
  fi
  rm -rf -- "$work" || true
  if (( docker_config_was_set )); then
    export DOCKER_CONFIG="$previous_docker_config"
  else
    unset DOCKER_CONFIG
  fi
}
trap cleanup_candidate_bootstrap EXIT

persist_stage="$(mktemp -d "$capacity_candidate_output_dir/.candidate-images.XXXXXX")"
docker_config="$work/docker-config"
install -d -m 0700 "$docker_config"

export DOCKER_CONFIG="$docker_config"
docker --config "$DOCKER_CONFIG" login "$capacity_registry_server" \
  --username "$capacity_registry_username" \
  --password-stdin <"$capacity_registry_password_file" >/dev/null

run_json="$(gh api \
  "repos/$GITHUB_REPOSITORY/actions/runs/$CANDIDATE_IMAGE_RUN_ID")"
jq -e --arg sha "$CANDIDATE_SHA" '
  .run_attempt == 1 and .head_sha == $sha and .head_branch == "main" and
  .path == ".github/workflows/orchestrator-candidate-images.yml" and
  .conclusion == "success"
' <<<"$run_json" >/dev/null

for artifact in \
  orchestrator-candidate-image-control-plane \
  orchestrator-candidate-image-agent \
  orchestrator-candidate-image-capacity-fixture \
  orchestrator-candidate-image-provenance
do
  gh run download "$CANDIDATE_IMAGE_RUN_ID" \
    --repo "$GITHUB_REPOSITORY" --name "$artifact" --dir "$work/$artifact"
done

python3 deploy/ops/verify-orchestrator-image-provenance.py \
  --root "$work" \
  --candidate-sha "$CANDIDATE_SHA" \
  --repository "$GITHUB_REPOSITORY" \
  --workflow-run-id "$CANDIDATE_IMAGE_RUN_ID" \
  --github-env "$work/candidate-images.env" \
  --record-output "$work/candidate-image-provenance.json"

set -a
. "$work/candidate-images.env"
set +a
verify_args=(
  --repo "$GITHUB_REPOSITORY"
  --cert-oidc-issuer https://token.actions.githubusercontent.com
  --signer-workflow "$GITHUB_REPOSITORY/.github/workflows/orchestrator-candidate-images.yml"
  --source-ref refs/heads/main
  --source-digest "$CANDIDATE_SHA"
  --deny-self-hosted-runners
)
for reference in \
  "$ORCHESTRATOR_GATE_CONTROL_PLANE_IMAGE" \
  "$ORCHESTRATOR_GATE_AGENT_IMAGE" \
  "$ORCHESTRATOR_GATE_FIXTURE_IMAGE"
do
  DOCKER_CONFIG="$docker_config" \
    gh attestation verify "oci://$reference" "${verify_args[@]}"
done

install -m 0600 "$work/candidate-image-provenance.json" \
  "$persist_stage/candidate-image-provenance.json"
install -m 0600 "$work/candidate-images.env" \
  "$persist_stage/candidate-images.runner-bindings.env"

python3 - \
  "$capacity_runner_gate_env_base_file" \
  "$work/candidate-images.env" \
  "$persist_stage/runner.env" <<'PY'
import os
import pathlib
import sys

EXPECTED_BASE = {
    "ORCHESTRATOR_GATE_EXPECTED_RUNNER_UNIT",
    "ORCHESTRATOR_GATE_ENVIRONMENT_ARGV_JSON",
    "ORCHESTRATOR_GATE_RESTART_ARGV_JSON",
}
EXPECTED_CANDIDATE = {
    "ORCHESTRATOR_GATE_CONTROL_PLANE_IMAGE",
    "ORCHESTRATOR_GATE_AGENT_IMAGE",
    "ORCHESTRATOR_GATE_FIXTURE_IMAGE",
    "ORCHESTRATOR_GATE_IMAGE_WORKFLOW_RUN_ID",
    "ORCHESTRATOR_GATE_IMAGE_PROVENANCE_RECORD_SHA256",
}


def canonical_lines(path: pathlib.Path, expected: set[str]) -> list[str]:
    raw = path.read_bytes()
    if not raw or len(raw) > 65536 or b"\0" in raw:
        raise SystemExit(f"{path} is empty, oversized, or contains NUL")
    lines = raw.decode("utf-8").splitlines()
    values: dict[str, str] = {}
    for line in lines:
        name, separator, value = line.partition("=")
        if separator != "=" or not value or name not in expected or name in values:
            raise SystemExit(f"{path} does not contain the exact expected bindings")
        values[name] = value
    if set(values) != expected:
        raise SystemExit(f"{path} does not contain the exact expected bindings")
    return [f"{name}={values[name]}" for name in sorted(values)]


base = canonical_lines(pathlib.Path(sys.argv[1]), EXPECTED_BASE)
candidate = canonical_lines(pathlib.Path(sys.argv[2]), EXPECTED_CANDIDATE)
output = pathlib.Path(sys.argv[3])
output.write_text("\n".join(base + candidate) + "\n", encoding="utf-8")
os.chmod(output, 0o600)
PY

jq -n \
  --arg candidate_sha "$CANDIDATE_SHA" \
  --arg control_plane "$ORCHESTRATOR_GATE_CONTROL_PLANE_IMAGE" \
  --arg agent "$ORCHESTRATOR_GATE_AGENT_IMAGE" \
  --arg fixture "$ORCHESTRATOR_GATE_FIXTURE_IMAGE" \
  --arg provenance_file "$capacity_image_provenance_record_file" \
  --arg runner_env_file "$capacity_runner_gate_env_file" \
  '{
    capacity_candidate_sha: $candidate_sha,
    capacity_control_plane_image: $control_plane,
    capacity_agent_image: $agent,
    capacity_fixture_image: $fixture,
    capacity_image_provenance_record_file: $provenance_file,
    capacity_runner_gate_env_file: $runner_env_file
  }' >"$persist_stage/candidate-images.vars.json"
chmod 0600 "$persist_stage/candidate-images.vars.json"

# Each rename publishes one complete mode-0600 file from a staging directory
# on the same protected filesystem. Publish the Ansible vars file last.
mv -f -- "$persist_stage/candidate-image-provenance.json" \
  "$capacity_image_provenance_record_file"
mv -f -- "$persist_stage/candidate-images.runner-bindings.env" \
  "$capacity_runner_candidate_bindings_file"
mv -f -- "$persist_stage/runner.env" "$capacity_runner_gate_env_file"
mv -f -- "$persist_stage/candidate-images.vars.json" \
  "$capacity_candidate_vars_file"
rmdir -- "$persist_stage"
persist_stage=""
cleanup_candidate_bootstrap
trap - EXIT
```

The login writes only to the mode-`0700` temporary Docker configuration and
reads the protected credential through stdin; it never expands the password in
argv or command output. `gh attestation verify` receives that same
`DOCKER_CONFIG`, so private OCI subjects are resolved through the authenticated
context that was just proven. The exit trap removes temporary credentials and
staging on both success and failure. Do not unset or replace `DOCKER_CONFIG`
between the login and the three OCI verification calls.

The protected `runner-base.env` input must contain exactly the expected runner
unit, repository-owned environment observer argv and protected restart-wrapper
argv. After all three attestations pass, the same command block atomically
publishes the verified provenance record, a five-line candidate-only runner
binding file, the complete eight-line `runner.env`, and
`candidate-images.vars.json`. The JSON file contains the three digest references
and the exact persistent provenance/runner paths, but no Registry username,
password or token. It is the final extra-vars input for Ansible; publishing it
last prevents a partially staged bootstrap from being selected.

GHCR Container packages are private by default. Configure
`capacity_registry_server`, `capacity_registry_username` and the controller-only
`capacity_registry_password_file` with a read-only package credential. The
playbook writes a root-only Docker client configuration for outer Compose pulls
and a separate strict, read-only Agent credential document. The Agent passes
that credential only as `X-Registry-Auth` for an immutable image whose registry
host matches the configured entry; credentials never enter Catalog, Job,
Operation or evidence payloads.

## Run

The Ansible controller needs Python 3, Rust/Cargo, Docker tooling, Ansible,
`jq` and a GitHub CLI with the attestation flags used above. The playbook pins
Actions Runner 2.336.0, GitHub CLI 2.97.0, and jq 1.7.1 by official download
URL plus SHA-256, then verifies their exact versions and required flags.
The ten workers, control plane, database and runner must already have mutually
reachable private addresses and correctly issued DNS certificates.

```bash
cd deploy/capacity
ansible-playbook -i /protected/inventory.yml \
  --extra-vars @/protected/capacity-vars.yml \
  --extra-vars @/protected/orchestrator-capacity/candidate-images.vars.json \
  site.yml
```

The execution sequence is fail-closed:

1. validate host separation, protected inputs, digests, candidate identity and
   verified provenance identity;
2. generate and self-verify the signed 20-service Catalog;
3. start TLS PostgreSQL, run expand-only migrations and start the single
   control plane;
4. read the pulled OCI revision label, check readiness build identity, and
   prove both external projection providers plus the Auth workload issuer over
   authenticated HTTPS;
5. issue 100 one-time registration codes through `/api/v1`, including the
   canonical Docker/Linux/x86_64 Node labels, then redeem them into 100
   independent Agent identities. Each code is staged as UID/GID 65532 mode
   `0600` inside only that Agent's mode `0700` bootstrap directory, mounted
   read-only into only that enrollment container. Enrollment passes the exact
   provisioned Node ID, persists a CSR/private key bound to the code digest,
   control-plane origin and CA before its first request, and reuses that CSR if
   the committed response is lost. Trying another code archives the complete
   prior pending key by code digest, so an invalid replacement cannot destroy a
   committed request's recovery material. The control plane replays the exact
   original certificate only for the same CSR while that certificate is active
   and valid; different-CSR, revoked, not-yet-valid and expired replays fail.
   Before reporting `ENROLLED` or `RECOVERED`, the Agent proves the exact
   Node/SPIFFE/serial through the read-only mTLS identity endpoint and only then
   publishes the monotonic current pointer and a no-private-key completion
   marker. The code is removed from both worker and controller only after that
   fully validated result;
6. install 20 real containers on every Node through Store operations;
7. create and apply a deterministic topology with 2,000 Endpoints and 8,000
   Links;
8. require all Nodes Ready, exactly 2,000 healthy Running Deployments, an
   `IN_SYNC` topology with zero drift, and successful real Operation targets on
   at least 50 distinct Nodes.
9. list every container in all 100 Engines, batch-inspect all 2,000 container
   objects, and require exactly 20 per Engine. Each must be Running, Docker
   healthy, candidate RepoDigest/image ID bound, carry the expected
   Deployment/Service/Node labels, and exclusively publish `8080/tcp` through
   `0.0.0.0:20000..20199` for its Engine/service slot. The Compose layer maps
   the same disjoint ranges to each worker host. The 10 worker observations
   must complete inside one fresh 90-second aggregate window.

If a persisted install or topology Operation is already `FAILED`, seeding uses
the official `/api/v1/operations/{id}:retry` action once with a new key derived
from the candidate, Operation identity and next generation. It verifies the
same Operation ID and an exact generation increment. `NEEDS_ATTENTION`,
`CANCELLED`, `ROLLED_BACK`, a second failure or an ambiguous retry response
fails closed and requires operator reconciliation.

An outer playbook retry may only resume an Operation whose persisted generation
is still zero. Once generation 1 exists, the environment provisioner observes
that same Operation to a terminal state and never turns a transient controller
or transport failure into generations 2..N. This keeps provisioning retries
idempotent across separate Ansible task attempts.

`orchestrator-capacity-environment.py` accepts only the protected token-helper
argv contract. It launches 1-32 argv entries with `shell=False`; stdout must be
exactly `{access_token,expires_at}`, where `expires_at` is Unix epoch seconds
and has more than ten minutes remaining. It never accepts a production static
token.

## Protected GitHub environments

Create the `orchestrator-production-soak` GitHub Environment using repository
administration credentials. Configure:

- secrets `ORCHESTRATOR_GATE_CA_PEM` and
  `ORCHESTRATOR_GATE_TOKEN_ARGV_JSON`;
- variables `ORCHESTRATOR_GATE_OCI_REVISION` and
  `ORCHESTRATOR_GATE_PROVENANCE_COMMIT`, both derived from the verified
  candidate and both equal to the workflow SHA.

Do not configure `ORCHESTRATOR_GATE_ENVIRONMENT_ARGV_JSON` as a GitHub
Environment secret: the workflow deliberately does not read it from GitHub.
The playbook installs it, together with
`ORCHESTRATOR_GATE_EXPECTED_RUNNER_UNIT` and
`ORCHESTRATOR_GATE_RESTART_ARGV_JSON`, in the dedicated runner service
environment so every Job process inherits one authoritative repository-owned
observer, protected restart wrapper and exact systemd unit identity. The
workflow deliberately does not overwrite either argv. The same runner
environment carries the three digest image references, candidate image
workflow run ID and combined provenance-record SHA-256 shown in
`group_vars/all.example.yml`. This split keeps provider access material on the
isolated runner while the two candidate authorization values remain
Environment-scoped and visible in workflow evidence.

The restart argv must name the protected wrapper installed under the runner's
fingerprinted `protected/` tree; it must not point directly at
`ansible-playbook`. The wrapper uses argv-only execution of
`restart-control-plane.yml` and the protected inventory. The capacity harness
also uses `shell=False`, requires a visible readiness interruption, recovery
within 60 seconds and persistence of an in-flight Operation.

`ORCHESTRATOR_GATE_ENVIRONMENT_ARGV_JSON` must be the exact argv rendered as
`capacity_environment_observer_gate_argv_json`. It invokes the repository-owned
`orchestrator-capacity-live-evidence.py`; it is not an operator-supplied black
box. Ansible installs that program, `live-evidence.yml`, the Engine collector,
the public-API/network verifier and a strict `config.json` under the runner's
mode-`0700` observer directory. Only the protected inventory, SSH connection
extra-vars, CA and token-helper argv vary by provider. The observer runs child
programs directly with `shell=False`, redacts their output, and has an 82-second
internal deadline (the gate allows at most 85 seconds).

Each invocation recollects all 100 Engines/2,000 containers, then independently
performs HTTP `GET /health` against all 2,000 Endpoints and source-side
`GET /probe?target=...` for all 8,000 Links. Redirects are not followed,
responses are capped at 4,096 bytes, individual requests time out after two
seconds, and candidate/service/source/target identities must match exactly;
TopologyStatus zero drift is an additional condition, never a substitute for
these network requests. The helper stdout is a strict, non-secret JSON object
containing counts, timestamps, an immutable image, resource-set SHA-256 values
and a protected-configuration fingerprint.

The fixture itself listens on container port 8080. `/health` returns the exact
candidate and service identity. `/probe` accepts only an IPv4 capacity target
inside ports 20000-20199, performs a raw fixed-path HTTP request with no DNS or
redirect handling, caps the complete target response at 4,096 bytes and rejects
timeouts or identity mismatches. The observer's network object has the stable
fields `checked_at_epoch_seconds`, `endpoint_checks_total/healthy/failed`,
`link_probes_total/healthy/failed`, `drift`, `endpoint_ids_sha256`,
`link_ids_sha256` and bounded `failure_samples`. After the independent network
pass it waits at most 180 seconds for the reconciler's bounded probe batches to
converge; a stale Runtime endpoint/release requires controlled reprovisioning.

The gate invokes it immediately before and after the mandatory control-plane
restart, once at the post-warmup/pre-soak boundary, once for every five-minute
Operation round, and once at the end. Report v2 indexes the third sidecar
`<report-stem>.environment.ndjson` as `environment_observations_ndjson`;
production therefore has exactly `pre_restart + post_restart + soak_boundary +
288 rounds + final = 292` ordered records. The boundary environment record and
Prometheus snapshot are checkpointed together before the first soak Operation.
The gate keeps collecting Prometheus/runner samples while the boundary helper
runs, so the report's global warmup/boundary/soak sample gap includes the full
helper window and may never exceed 90 seconds. Every production sample and its
Prometheus sidecar carry an absolute `sample_clock_seconds` value from the same
runner `CLOCK_BOOTTIME`; the report summary and GA validator compute continuity
directly from that field. `sampled_at_epoch_seconds` remains correlation
metadata and is never the gap truth, even when its BOOTTIME offset is inside the
allowed clock-correlation tolerance. After warmup, no environment observation
gap may exceed 390 seconds. Workflow dispatch, report lifetime,
samples, helper observations and checkpoints are also bound to one runner
BOOTTIME timeline. Any helper timeout, stale worker window, timeline gap,
count/health/drift failure or resource/configuration fingerprint change fails
the run.

The runner's protected `.env` is for helper credentials and endpoints, not for
the two candidate identity variables above. Those remain Environment-scoped so
the workflow records exactly which protected values authorized the run.

After the dedicated Actions `Runner.Listener` process has been continuously
active for at least one hour, dispatch the first attempt of
`orchestrator-capacity.yml` from `main` with `profile=production` and
`soak_seconds=86400`; workflow reruns are rejected. The harness requires
`ORCHESTRATOR_GATE_EXPECTED_RUNNER_UNIT` to equal the protected exact `.service`
value, binds `RUNNER_NAME` to that same current process service, and requires the
systemd ControlGroup to end in that exact unit, then follows the current Job's
`/proc` PPID chain to its real `Runner.Listener` ancestor. The authenticated
Actions API `Date` and workflow `created_at`, combined with monotonic process
ages, prove both the unit and Listener were active for one hour before dispatch;
the local wall clock is only a bounded sanity check. Every 30-second
warmup/soak sample and the final checkpoint must retain the same boot ID,
ControlGroup, unit, InvocationID, MainPID, Listener PID/start ticks and observer
process identity in `active/running` state. BOOTTIME reads are bracketed by two
MONOTONIC reads so scheduler preemption cannot resemble suspend, while a real
BOOTTIME-MONOTONIC offset change still fails the gate. Host `/proc/uptime`, a
service-wrapper-only proof, a rerun or a missing observation is not accepted.
Any code or image fix creates a new SHA and requires the entire deployment and
soak sequence again.

Use the successful first-attempt candidate-image run for the exact SHA. The
production workflow rejects an omitted, stale or different-commit run ID:

```bash
export GITHUB_REPOSITORY=OWNER/REPOSITORY
export CANDIDATE_SHA="$(git rev-parse HEAD)"
export CANDIDATE_IMAGE_RUN_ID=REPLACE_WITH_VERIFIED_RUN_ID

run_json="$(gh api \
  "repos/$GITHUB_REPOSITORY/actions/runs/$CANDIDATE_IMAGE_RUN_ID")"
jq -e --arg sha "$CANDIDATE_SHA" '
  .run_attempt == 1 and
  .head_sha == $sha and
  .head_branch == "main" and
  .path == ".github/workflows/orchestrator-candidate-images.yml" and
  .conclusion == "success"
' <<<"$run_json" >/dev/null

gh workflow run orchestrator-capacity.yml \
  --repo "$GITHUB_REPOSITORY" \
  --ref main \
  -f base_url=https://orchestrator-capacity.example.com:8090 \
  -f profile=production \
  -f soak_seconds=86400 \
  -f candidate_image_run_id="$CANDIDATE_IMAGE_RUN_ID"
```

The playbook registers pinned Actions Runner 2.336.0 with `--disableupdate`.
Immediately before the first `config.sh` call it executes
`capacity_github_runner_registration_token_argv_json` on the Ansible controller
with `shell=False`. The helper stdout must be exactly
`{"token":"...","expires_at":<unix-seconds>}`, no larger than 4,096 bytes,
with 10-60 minutes remaining. A static token collected before the database,
control plane and workers are configured is not accepted.

The runner identity fingerprint covers the expected version, archive URL and
SHA-256, repository, runner name, exact labels, work directory, update policy,
service user and installation directory. Registration is journaled before
`config.sh`; after a crash, the pending journal permits completing that same
identity, while an existing `.runner` with a different or unprovable identity
fails closed and requires controlled reprovisioning instead of silently reusing
old bits.

A separate configuration fingerprint covers that identity, pinned GitHub CLI
and jq identities, the protected `.env` and helper-file contents. The stale applied marker is removed before
changing those inputs. `.ojos-capacity-config-applied.sha256` and the applied
identity marker are committed only after a required restart completes and
`systemctl is-active` confirms the installed service is active. A retry after
service installation or protected-file copying therefore cannot lose a
pending restart. An unchanged active runner is not restarted and keeps
accumulating soak uptime; after any actual restart, wait for a fresh one-hour
baseline before dispatching production.

After every recursive helper copy, the playbook inventories the deployed
`protected/` tree as sorted relative paths and SHA-256 digests and requires it
to equal the protected source manifest exactly. Symbolic links are rejected.
A stale, missing or digest-mismatched helper removes the applied configuration
marker and fails the play before handlers are flushed or a new marker can be
committed; remove the unexpected target entry and rerun the playbook.
