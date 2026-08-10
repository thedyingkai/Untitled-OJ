# Cross-machine Service Contract v2 gate

This gate models A as the control/business machine and B as the Judge Worker
machine.  Its execution levels are intentionally separate so a cheap contract
test can never be presented as live deployment evidence.

The only current-status record is
[`docs/completeness-summary.md`](../../docs/completeness-summary.md). Evidence
files are immutable run outputs: record their `run_id` and whole-file SHA-256,
and state whether the checkout was clean. A `PASSED` report produced from a
working tree with uncommitted changes is pre-commit evidence even when its
embedded build identity names the base commit. Two nested Engines on one Linux
host are production-equivalent functional isolation, not proof of two physical
machines, 100-node capacity, 24-hour stability, signed GA artifacts or a
security acceptance.

## Commands

Docker-free contract and validator tests:

```text
python -m unittest discover -s deploy/cross-machine/tests -p test_*.py -v
python deploy/cross-machine/cross_machine_e2e.py validate --repo-root .
```

Real two-Engine gate:

```text
python deploy/cross-machine/cross_machine_e2e.py live --repo-root . --evidence artifacts/cross-machine/live-evidence.json
```

Strict production-component gate:

```text
python deploy/cross-machine/cross_machine_e2e.py live --repo-root . --evidence artifacts/cross-machine/full-evidence.json --full-components
```

`live` always creates two privileged `docker:dind` containers and proves their
Engine IDs, outer container IDs, and storage isolation with mutually invisible
marker volumes.  A service subnet and an Agent subnet are separate inside the B
Engine.  `DOCKER-USER` permits the service subnet to reach only A's TLS Gateway;
the test then actively proves PostgreSQL, Redis, MinIO, the direct Judge port,
the control plane, and the OCI Registry are unreachable from a B business
container.  A B Agent probe must still reach the control plane and Registry.

The ordinary live gate uses a protocol fixture for the business state machine,
but it uses real Docker Engines, networks, TLS, read-only context volumes and
cross-Engine containers.  The third-party provider/consumer route is generated
from the two Release v2 JSON manifests and the Topology link; neither
Orchestrator nor Gateway product code contains that API ID.

`--full-components` has no fixture fallback. It builds and starts the production
Orchestrator control plane with TLS PostgreSQL, OIDC, a trusted signed Catalog,
and the real A-side Gateway, Auth, Problem, Judge, Storage, and Redis services.
It enrolls the real Agent over mTLS, validates and installs Judge Worker through
the Store API, and requires the Agent to create the B-side container. The Store
request names the target Node and confirmed API bindings but supplies no
endpoint; the logical endpoint is derived from current Node facts and the
backend-worker Release publishes no Docker port.

Pre-infrastructure images are exported as one target-specific multi-image
archive for A and one for B. Each nested Engine performs exactly one
`docker image load`, so shared layers are unpacked only once per Engine. The
later Catalog image is built directly inside A after RepoDigests exist. The
default archive save and vfs load timeouts are 3600 and 7200 seconds;
constrained runners may override them with
`OJOS_CROSS_MACHINE_IMAGE_BUNDLE_SAVE_TIMEOUT_SECONDS` and
`OJOS_CROSS_MACHINE_IMAGE_BUNDLE_LOAD_TIMEOUT_SECONDS` (both are capped at six
hours).

Host-side `docker build` commands make at most four attempts, with 2, 5 and 15
second backoffs. Retries are limited to explicit TLS, TCP, DNS, Registry
overload and image-layer short-read/EOF signatures. Dockerfile, compilation,
configuration, authorization and other deterministic failures fail on the
first attempt. Retry evidence contains only the build label, classified cause
and an error fingerprint; raw command errors are not copied into evidence.

The strict flow then creates a problem through the public API, observes its
transactional outbox and Judge projection, submits code, and requires the real
Worker to claim through the Gateway, download source and package resources by
binding-relative SHA-256 references, execute them with nsjail, and report the
result. Evidence includes the Operation/job/lease, OCI RepoDigest and fixed
HostConfig, read-only ServiceContext and credential generation, database state,
network/volume isolation, and a Topology-only binding reconfiguration that must
increment the context generation without replacing the Worker container.

## Failure and skip semantics

The Docker-free test does not claim live coverage.  The live commands never
skip: a missing Docker CLI, unreachable daemon, non-Linux daemon, unavailable
privileged dind, missing iptables policy, failed image pull, or incomplete
evidence exits non-zero and writes a `FAILED` evidence document.  The evidence
validator rejects `SKIPPED`, `RUNNING`, partial, one-Engine, fixture-only (when
`--require-full` is used), or otherwise incomplete documents.

Failure-log collection and cleanup are bounded, best-effort closing phases.
Their errors are recorded separately as `failure_log_errors` and
`cleanup_errors`; they never replace the original scenario failure. A scenario
that passes but cannot be cleaned up still exits non-zero and atomically writes
terminal `FAILED` evidence. `PASSED` is written only after successful cleanup.

The GitHub workflow runs the Docker-free contract gate for pull requests and
main. Live execution is an explicit `workflow_dispatch` choice because nested
Engines and the production component images are expensive; not dispatching it
produces no live evidence and is not counted as a live pass. A strict document
must also pass the explicit verifier:

```text
python deploy/cross-machine/cross_machine_e2e.py verify-evidence artifacts/cross-machine/full-evidence.json --require-full
```
