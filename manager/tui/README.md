# Orchestrator v1 TUI

The production TUI is a `/api/v1` client. It does not open a browser, execute
control-plane mutations in process, or accept a bearer token from an
environment variable.

## Remote sign-in

Register a public OIDC client that allows the OAuth 2.0 Device Authorization
Grant, then run:

```powershell
cargo run -p ojos-orchestrator-tui -- `
  --api-url https://orchestrator.example `
  --oidc-issuer https://identity.example `
  --oidc-client-id ojos-orchestrator-tui `
  --oidc-audience ojos-orchestrator
```

The same values can be supplied as `OJOS_ORCHESTRATOR_URL`,
`OJOS_TUI_OIDC_ISSUER`, `OJOS_TUI_OIDC_CLIENT_ID`,
`OJOS_TUI_OIDC_SCOPE`, and `OJOS_TUI_OIDC_AUDIENCE`. The TUI displays the
provider's verification URI and user code, follows `interval`/`slow_down`, and
keeps the resulting access token only in process memory.

For automation of one TUI command, add `--command`, for example:

```powershell
ojos-orchestrator-tui --api-url https://orchestrator.example `
  --oidc-issuer https://identity.example `
  --oidc-client-id ojos-orchestrator-tui `
  --command "node health edge-1"
```

Run `--help` or enter `:` in the interactive TUI for the full command surface.
It covers Catalog/Store, versioned Topology, durable Operation logs/events,
Node identity and lifecycle, Deployment lifecycle, and Diagnostics. Mutations
are capability-gated and carry `Idempotency-Key`; topology concurrency uses
strong `If-Match` ETags; collection commands accept the returned cursor.

`--legacy-local` is an explicit, deprecated 0.2 compatibility console. It is
never selected automatically and is not a production v1 authentication path.
