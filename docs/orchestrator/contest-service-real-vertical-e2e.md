# Contest service real vertical PR gate

`deploy/ops/contest-service-real-vertical-e2e.sh` is the required live proof
that the reference service is more than a compiler/control-plane fixture. The
script fails closed when Docker, TLS tooling, PostgreSQL, Redis, the registry,
or the real Gateway executable is unavailable. The Rust test only skips during
ordinary `cargo test`; the script sets `OJOS_REQUIRE_CONTEST_REAL_VERTICAL_E2E=1`
and treats every missing input as a test failure.

| Capability | Real vertical gate | Existing contest clean-room | Existing Docker Agent lifecycle |
| --- | --- | --- | --- |
| Signed Service Contract v3 and complete artifact graph | Yes | Yes, deterministic fixture digests | No |
| Authenticated digest-pinned runtime and migration OCI pulls | Yes; successful one-shot migration identity is retained in the Agent ledger receipt after Docker cleanup | No | Yes, synthetic runtime |
| PostgreSQL ResourceClaim, generated role/DB/DSN, migration OCI | Yes | Protocol receipt only | No |
| Agent materialized Service Context, ApiBindings, workload JWT and Redis event context | Yes | Protocol model | Runtime context only |
| Real contest binary and CRUD through real Gateway routes | Yes | No | No |
| Gateway and service operation-level permission allow/deny | Yes | Projection only | No |
| Transactional outbox and Redis Stream publication | Yes | Contract only | No |
| User/admin frontend signed digest allowlist | Yes | Snapshot metadata | No |
| Upgrade, cancel/ABORT, rollback and topology-safe uninstall | Install plus exact-container test cleanup; control-plane lifecycle remains in the adjacent gates | Yes | Upgrade/rollback/uninstall |
| Agent restart/completion-loss replay | No | No | Yes |

The three gates are intentionally complementary. Lifecycle assertions remain
in the clean-room and synthetic Docker tests; the real vertical gate is the PR
proof that their critical resource, migration, binding, Gateway, service, and
event effects are executable together.

The live Agent driver itself executes as the standard-v3 workload identity
`65532:65532`, with only the Docker socket GID as a supplemental group. Its
service-context and ResourceClaim bind sources therefore remain `0700`/`0600`
without root, Linux capabilities, or `CAP_CHOWN`. Capacity deployment uses the same identity and
mounts each Agent's isolated state root read-only at the identical absolute
path inside only its paired Docker Engine, because bind sources are resolved
in the daemon namespace.

The Gateway, Orchestrator, Agent and contest runtime in this gate are production
binaries. Auth and Problem are deliberately bounded protocol mocks: Auth issues
real Ed25519 workload JWTs, answers permission checks and consumes topology
projection protocol; Problem answers the signed provider API used by the real
contest binary. The event assertion joins this CRUD's exact contest ID to its
PostgreSQL outbox event ID, waits for `published_at`, and matches that same
typed CloudEvent in Redis, so pre-existing stream entries cannot satisfy it.
The frontend assertion proves both user/admin signed module
metadata, Gateway allowlisting and exact bundle bytes. Actual Vue host loading,
isolation and `dispose()` behavior remain covered by the dedicated browser gate.
