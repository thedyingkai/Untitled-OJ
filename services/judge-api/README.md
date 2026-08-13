# Judge API

`judge-api` stores submissions, judge tasks, leases, problem projections and result outbox records in its claimed PostgreSQL database. PostgreSQL is the business authority; Redis Streams only accelerate task wakeups and carry the current shared event transport. A failed Redis `XADD` never changes a successfully persisted submission or task to `SYSTEM_ERROR`, and Workers continue claiming eligible PostgreSQL tasks without a Redis task ID.

## Service Contract v3

The hand-maintained entry points are `ojos.service.yaml`, `config.schema.json`, `api/*.openapi.yaml`, `events/*.schema.json` and the two `frontend/*/manifest.json` files. `gen/**` is compiler output and must be regenerated with `ojos service generate`; CI checks it byte-for-byte.

Runtime provider paths are intentionally stable:

- user and admin operations are registered at `/judge/**` and are externally mounted at `/api/judge/**` by the Contribution route compiler;
- Worker control is provider-native at `/api/judge/worker/**`; `/judge/worker/**` remains a one-version development compatibility alias;
- `/healthz` is liveness, while `/readyz` verifies PostgreSQL, event transport, the Agent Service Context, required ApiBindings and workload credential.

Every user/admin operation performs the same `judge.*` permission check declared by OpenAPI. Worker routes require a Gateway-bound `judge-worker` workload identity in managed deployments; the shared Worker token is an explicit unmanaged development compatibility mode only.

## Managed resources and dependencies

The `submissions` `postgresql.database/v1` claim is materialized by the Agent at `/run/ojos/resources/submissions/dsn`. Both the runtime and `judge-migration-v1` image read this output; neither accepts a control-plane plaintext DSN. The migration image exposes the stable `/ojos-migrate` entry point and applies all checked-in `00000*.up.sql` files in lexical order under an advisory lock, recording each migration transactionally.

Managed runtime calls address `auth.user.permission.check` and `storage.object.get/put/head` by exact requirement ID through the hot-reloading ContextProvider. Direct service URLs, global service tokens, `REDIS_URL`, local submission files, legacy problem package directories and shared Worker tokens are restricted to unmanaged development. This makes the production image compatible with a read-only root filesystem; the Agent supplies only read-only context/resource mounts and the workload's ephemeral writable mounts.

Database lifecycle defaults to `RETAIN`. Uninstall removes bindings and the runtime, not the claimed database; destructive removal requires the platform's separately audited purge action.

## Local verification

From the repository root:

```powershell
cargo run -p ojos-service -- service generate services/judge-api/ojos.service.yaml
cargo run -p ojos-service -- service check services/judge-api/ojos.service.yaml --generated
Push-Location services/judge-api
go test -race ./...
go vet ./...
go build ./...
node frontend/bundle_test.mjs
& scripts/resolved-artifacts-fixture.test.ps1
& scripts/publish-fixture.test.ps1
Pop-Location
```
