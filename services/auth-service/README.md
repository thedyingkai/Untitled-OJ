# Auth Service

## One-time initial administrator bootstrap

A new Auth database contains the `super_admin` role but deliberately does not
assign it to a user. To initialize a deployment without forging a JWT or writing
directly to PostgreSQL, start Auth with exactly one dedicated bootstrap secret:

- `AUTH_ADMIN_BOOTSTRAP_SECRET_FILE=/run/secrets/ojos-auth-admin-bootstrap` (preferred),
  or
- `AUTH_ADMIN_BOOTSTRAP_SECRET=<random value>` for an isolated test harness.

The production secret file must be a non-symlink regular file, non-empty and
bounded to a 32 through 512 character URL-safe token (`A-Z`, `a-z`, `0-9`, `_`,
`-`), owned by the exact Auth runtime uid/gid `65532:65532`, with exact mode
`0600`. The token must differ from every JWT,
internal, observability, management, workload, Contribution ACK, and service
credential. Configuring both
sources, a weak/reused secret, a missing/non-regular file, or enabling bootstrap
in `OJOS_SMOKE_MODE` causes Auth to refuse startup. With neither source
configured, the bootstrap route is not registered.

After migration `000014_initial_admin_bootstrap` and Auth startup, make one
request through the same HTTPS Gateway used for Auth traffic. The standard
Gateway exposes it as `/api/auth/bootstrap/admin` and strips `/api`; direct Auth
tests use `/auth/bootstrap/admin`:

```text
POST /api/auth/bootstrap/admin
X-OJOS-Bootstrap-Secret: <dedicated one-time secret>
Content-Type: application/json

{"username":"initial-admin","email":"admin@example.test","password":"..."}
```

Success is HTTP `201` with:

```json
{"code":0,"msg":"success","data":{"user_id":1,"username":"initial-admin"}}
```

Auth creates the user, assigns `user` and `super_admin`, appends the
`auth.bootstrap.initial_admin` audit event, and consumes the durable bootstrap
marker in one serializable PostgreSQL transaction. Concurrent requests have one
winner. Every later request returns HTTP `409`/code `40931`, including after an
Auth restart. If an upgraded database already contains a system-scoped super
administrator, migration `000014` consumes the marker and bootstrap refuses with
`40931`; removing that administrator does not reopen bootstrap.

After success, remove both the `AUTH_ADMIN_BOOTSTRAP_SECRET_FILE` environment
entry and its `/run/secrets/ojos-auth-admin-bootstrap` read-only bind mount from
the production Compose deployment, delete the host token file, and recreate
Auth (`docker compose up -d --force-recreate auth-service`). A subsequent
request to `/api/auth/bootstrap/admin` must return `404`; this proves the route
was not registered after restart. Obtain the administrator JWT only via
the ordinary login contract:

```text
POST /auth/login
Content-Type: application/json

{"username":"initial-admin","password":"..."}
```

The bootstrap secret is not a JWT, internal bearer, workload credential, or
recovery credential. Losing it before initial bootstrap requires an explicit
operator replacement and Auth restart. It cannot be used to create another
administrator after the database marker is consumed.
## Service Contract v3

`ojos.service.yaml` and `api/*.openapi.yaml` are the developer-owned Service
Contract v3 inputs. `gen/` is deterministic compiler output; regenerate it with
`ojos service generate` and verify it with `ojos service check --generated`.

Managed installations fail closed unless the Agent supplies:

- the `auth` PostgreSQL resource output at
  `/run/ojos/resources/auth/dsn` (or `OJOS_RESOURCE_AUTH_OUTPUT_FILE`);
- Service Contract config values through `OJOS_CONFIG_*`;
- JWT, management, workload signing, and Orchestrator projection secrets
  through `OJOS_SECRET_*`.

Managed mode never reads `DATABASE_URL`, legacy Auth tokens, or a private key
path as a fallback. Those aliases remain only for explicit unmanaged local
development. The runtime exposes `/healthz` and `/readyz`; readiness verifies
the claimed database and all managed control-plane components.

`migrations/Dockerfile` is the signed one-shot PostgreSQL migration runner.
It uses the same Agent resource output, an advisory lock, and a durable
migration ledger. Release publishing fixtures use an ephemeral Ed25519 key and
temporary Catalog directory; no test key or generated Catalog is persisted.
