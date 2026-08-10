# Judge Worker deployment paths

`docker-compose.yml` is a development compatibility entry only. It uses the
legacy `OJOS_JUDGE_API_URL` and shared `OJOS_WORKER_TOKEN` variables and must
not be used as the production B-machine deployment procedure. It is also
disabled by default; local compatibility runs must add
`--profile legacy-development` explicitly.

For production, install Orchestrator Agent on B, enroll the node, and allow the
exact signed `judge-sandbox-v1` profile digest and exact Worker OCI artifact in
the Agent's local policy. Start from
[`runtime-policy.example.json`](runtime-policy.example.json), replace its
`registry.example/...@sha256:...` value with the `repository@sha256` selected
from the trusted Catalog, and pass that JSON through the Agent's
`--runtime-policy` option. A repository wildcard, floating tag, bare digest,
or a different profile digest is rejected before Docker pull/create.
Treat `services/judge-worker/release.yaml` as the source template: its
`local://` source and empty image/checksum are not production-installable. Use
the Catalog generator to bind that manifest to a signed metadata package and
an exact `repository@sha256`, import the trusted Catalog, then select that
release in Store and confirm its `judge_control` and `storage_get` ApiBindings.
The Agent creates the container,
mounts `/run/ojos/service` read-only, rotates the Deployment JWT, and validates
Docker health before the deployment can become `Running/Healthy`.

The example's `service_context_root`/`judge_sandbox.context_root` is the only
operator-selected Agent data root. For each Deployment the Agent derives a
digest-named private directory below it; the read-only service context lives
at `<root>/<deployment-digest>/service`, and Worker scratch data lives at
`<root>/<deployment-digest>/work`. Cache is the Agent-created Docker volume
`ojos-judge-cache-<deployment-digest>` and its physical host directory remains
Docker-managed. Releases and install requests cannot supply any of these host
paths or override capabilities/mounts/security options.

The Agent creates that release-scoped cache volume explicitly before context
materialization, image pull and container creation. Its exact deployment,
service, artifact, runtime-profile and lifecycle ownership labels are written
to the Agent SQLite ledger. Failed installs compensate in reverse order; an
uninstall removes the container, then the owned cache volume, then the private
context tree. Recovery validates the persisted ownership labels before an
idempotent removal, so a same-named foreign volume is never adopted or deleted.

The B-machine Agent/host needs outbound access only to:

- the Orchestrator Agent API over node mTLS;
- the digest-pinned OCI registry used by the selected release.

The Worker business network needs only the A-machine HTTPS Gateway plus the
required DNS/time trust chain. The Worker does not call the Agent API and does
not pull from the Registry itself; the Agent/Docker daemon performs those
control-plane operations.

It does not receive PostgreSQL, Redis, MinIO, Auth administration, Gateway
administration, or global worker credentials. Business traffic goes through
the Gateway using the applied Topology bindings.

The standalone Agent also rejects Auth/Gateway/API Registry management
variables at startup. Those providers run only in the A-machine control
plane. `--legacy-release-providers` is a compatibility switch for local
development and is rejected unless `OJOS_ENVIRONMENT=development`; a v2
managed job is rejected even when that compatibility switch is enabled.

The full-stack `deploy/compose/docker-compose.yml` keeps its old in-process
Worker behind the explicit `legacy-development` Compose profile. Starting the
production profile does not start that Worker. To exercise the compatibility
path locally, opt in with `--profile legacy-development` and the development
override; this is not a B-machine deployment procedure.

Current implementation and evidence status is maintained only in
[`docs/completeness-summary.md`](../../docs/completeness-summary.md). In
particular, a local dual-Engine run is not evidence of two physical hosts or a
final commit-bound release.
