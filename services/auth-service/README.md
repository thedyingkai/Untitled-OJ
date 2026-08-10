# Auth Service

## One-time initial administrator bootstrap

A new Auth database contains the `super_admin` role but deliberately does not
assign it to a user. To initialize a deployment without forging a JWT or writing
directly to PostgreSQL, start Auth with exactly one dedicated bootstrap secret:

- `AUTH_ADMIN_BOOTSTRAP_SECRET_FILE=/run/secrets/ojos-initial-admin` (preferred),
  or
- `AUTH_ADMIN_BOOTSTRAP_SECRET=<random value>` for an isolated test harness.

The secret must contain 32 through 512 bytes and must differ from the JWT,
internal bearer, and workload control-plane credentials. Configuring both
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

Remove the bootstrap secret from the runtime configuration and restart Auth
after success; the route then disappears. Obtain the administrator JWT only via
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
