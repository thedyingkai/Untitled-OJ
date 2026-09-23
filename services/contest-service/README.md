# Contest Service reference vertical

This directory is the developer-owned reference service for the OJOS Service Contract v3 workflow. It deliberately does not contain a release signature or invented OCI digests.

Authoring and validation:

```text
cargo run -p ojos-service -- service generate services/contest-service/ojos.service.yaml
cargo run -p ojos-service -- service check services/contest-service/ojos.service.yaml --generated
cargo run -p ojos-service -- service build services/contest-service/ojos.service.yaml
```

The build command writes the exact unresolved artifact slots to `gen/build-input.json`. A trusted external builder must build the runtime OCI, migration OCI, frontend bundles, SBOM, and provenance, resolve their real digests, and only then invoke `ojos service publish` to produce a signed Catalog subject. Historical verification fixtures and ephemeral signing helpers are maintained outside the product repository; they are not production artifacts and must never be distributed, trusted, or installed. See the [service authoring boundaries](../../docs/architecture/service-authoring.md) for the current v2/v3 and packaging transition.

At runtime the PostgreSQL ResourceClaim output is read from `/run/ojos/resources/contests/dsn`; `CONTEST_DATABASE_SECRET_FILE` remains an unmanaged-development alias only. The service never accepts a database password in its control-plane config or command line. It uses the shared hot-reload ContextProvider for its required Problem API and permission-check bindings, and repeats the OpenAPI operation permission check inside the provider instead of trusting the Gateway alone. The shared typed-event codec plus transactional outbox publishes `contest-service.contest-created`. In a managed workload the runtime validates the Agent-projected Event Contract, reads only the Agent-local Redis connection file, and runs the shared outbox relay; missing or mismatched event context fails startup closed.

The resource lifecycle is `retain`: uninstall and compensation detach the claim but do not delete the database. Database deletion is outside this service and requires the platform's separately confirmed purge action.
