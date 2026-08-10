# Service Contract v2 credential boundary

Production separates three identities:

- the enrolled Node Agent uses its Node mTLS certificate only for the Agent API;
- each managed Deployment receives a short-lived workload JWT in its private,
  read-only service context;
- the A-machine control plane owns Auth/Gateway topology projection and the
  dedicated Auth workload-issuer credential.

A remote Agent must not contain `ORCHESTRATOR_AUTH_ADMIN_*`,
`ORCHESTRATOR_GATEWAY_ADMIN_*`, API Registry credentials, or the generic
external provisioner token. The Agent checks for those variables before
loading provider configuration and refuses to start if any are present. Auth,
Gateway, and API Registry steps in a v2 managed job are also rejected before
runtime side effects.

Auth workload issuance uses only this dedicated pair:

```text
ORCHESTRATOR_AUTH_WORKLOAD_ORIGIN
ORCHESTRATOR_AUTH_WORKLOAD_TOKEN
```

There is no fallback to an Auth admin token or the generic Orchestrator
internal token. Auth receives the same token as
`OJOS_WORKLOAD_CONTROL_PLANE_TOKEN`; it is separate from
`AUTH_INTERNAL_TOKEN`. Auth alone mounts the Ed25519 private key, while Gateway
mounts only the public key. Production startup fails when the signing or
verification key is absent, and the workload TTL is fixed at 900 seconds.

The Orchestrator materializes only the B-reachable HTTPS Gateway origin and CA
into the Deployment service context. The context never contains the workload
issuer token, Auth/Gateway admin tokens, Node private key, or API Registry
credentials.

The legacy Node-side release providers require both the explicit Agent flag
`--legacy-release-providers` and `OJOS_ENVIRONMENT=development`. The old
full-stack Compose Worker similarly requires the explicit
`legacy-development` profile. Neither mechanism is a production deployment
path.
