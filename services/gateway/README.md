# OJOS Gateway

The Gateway is a platform-reserved service with three responsibilities:

1. expose liveness/readiness, topology projection and authenticated platform
   administration routes;
2. consume the active deployment-scoped Contribution snapshot and proxy its
   operation routes;
3. serve verified, content-addressed frontend artifacts from the same origin.

`ojos.service.yaml` deliberately declares no `exposures` or `frontends`.
Publishing the Gateway as its own Contribution would make an external route
target the proxy that owns that route. The compiler output is therefore
required to contain zero Contribution routes, and the runtime also blocks any
snapshot route whose service is `gateway` or whose path overlaps a reserved
platform prefix.

`api/openapi.yaml` is the only maintained HTTP contract. The checked-in
`gateway.api` is retained solely as a compatibility scaffold for existing
go-zero handler types; it has no generation directive. Contract tests compare
every registered platform route, method, audience and permission with OpenAPI,
so it cannot silently become a second source of truth.

## Managed and compatibility modes

An Agent-managed process (`OJOS_MANAGED_WORKLOAD=1`) consumes only
compiler-materialized `OJOS_CONFIG_*` / `OJOS_SECRET_*` values, the mounted
Service Context, and the platform-injected workload public-key file. Image YAML
business routes, direct service URLs, compose service lists and artifact-root
paths are discarded before the proxy is constructed or routes are registered.
The Auth permission check is addressed by the required
`auth.user.permission.check` ApiBinding; the Orchestrator endpoint and its
credential remain the reserved bootstrap/control-plane channel.

`etc/gateway.yaml` still contains static business routes only for the existing
unmanaged Compose development profile. They are not a production truth and can
be deleted with that compatibility profile after every business service has
migrated to Contribution revisions.

The Gateway owns no SQL schema and therefore declares no database claim or
migration. Its Redis URL is a platform bootstrap secret in the configuration
schema: the current generic resource-claim API intentionally implements only
PostgreSQL, while Redis remains the topology projection's platform dependency.

## Verification

From the repository root:

```powershell
cargo run -p ojos-service -- service generate services/gateway/ojos.service.yaml
cargo run -p ojos-service -- service check services/gateway/ojos.service.yaml --generated
go test -race ./...
go vet ./...
go build ./...
```

Run the Go commands from `services/gateway`. The publish fixture uses an
ephemeral Ed25519 seed and deletes its scratch Catalog:

```powershell
& services/gateway/scripts/resolved-artifacts-fixture.test.ps1
& services/gateway/scripts/publish-fixture.test.ps1
```
