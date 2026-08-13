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

Service Contract v2 installs use the same explicit Binding selection as Web.
First run `store validate <service> <node> <version|-> <catalog|-> <channel|->
<requirement=provider-deployment,..|-> <topology@applied-revision|->
[pipeline-options.json]`; the response
shows every compatible candidate, recommendation, ambiguity, Node runtime
facts, and the selected closed Runtime Profile. Pass the confirmed mapping to
`store install` using the same arguments. `deployment bindings <deployment>`
(or `b` on the Deployments page) shows provider health, drift, context
generation, and credential generation. Both validation and installation require
an explicit applied Topology revision; the TUI never silently chooses a Topology
or among multiple healthy providers.

Append an optional `pipeline-options.json` argument to both `store validate` and
`store install` to express the complete release pipeline without putting JSON or
secret references into positional arguments. The same file must be reused for
validation and installation; it has this strict shape (omitted fields use the
shown defaults):

```json
{
  "start": true,
  "migration_policy": "APPLY",
  "gateway_node_id": "gateway-node-a",
  "config": {},
  "secret_refs": {
    "signing_key": "secrets/judge/signing-key"
  }
}
```

Use `-` for the Topology argument when a provider-only release has no Topology,
but a pipeline options file still follows it. `secret_refs` contains references
only; the TUI never accepts secret plaintext in this document.

For Composition releases, `store validate` adds a read-only, redacted summary
of the immutable plan, ResourceClaim providers, and unresolved inputs. The TUI
does not edit Composition inputs or install from that summary; use the Web
manager for the validate/edit/revalidate/install flow.

Upgrade and rollback read the Deployment's current active consumer Bindings
before submitting the replacement, so an omitted mapping cannot silently drop
them. An intentional rebind uses `store upgrade|rollback <deployment>
<version|-> <catalog|-> <requirement=provider,..|->
<topology@applied-revision,..|->`. Review the validation response's candidate
set, prospective Topology diff, and the locally added deterministic
`selection_fingerprint` before reusing the same explicit mapping for the
mutation. The fingerprint is a change detector, not a signature or trust proof.
Uninstall conflicts report the active
Topology/Link requirements that must be removed and applied first.

Permanent ResourceClaim deletion is intentionally available only through the
explicit remote command (there is no shortcut or free-form JSON editor):

```text
resource purge <claim-id> <node-id> <sha256-digest> <generation> \
  "PURGE <claim-id> <sha256-digest> GENERATION <generation>" "<audit reason>"
```

The TUI checks the confirmation byte-for-byte before making a request and sends
only the five fields in `ResourcePurgeRequest`. It never accepts a database
password, DSN, secret reference, or actor field for this action.

`--legacy-local` is an explicit, deprecated 0.2 compatibility console. It is
never selected automatically and is not a production v1 authentication path.
