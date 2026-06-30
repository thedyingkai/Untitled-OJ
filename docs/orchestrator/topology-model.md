# Topology Model

Topology is the Orchestrator view of currently known services and runtime connectivity.

It is built from:

```text
Service
Endpoint
Link
Operation
LogView
DiagnosticReport
```

Local deployment templates may seed preview endpoints and links when no persistent store is configured, but templates are not formal topology objects.

Endpoint identity is `ip:port:service-name`. Link identity is endpoint-to-endpoint:

```text
source endpoint -> target endpoint
```

`GET /topology` is rebuilt from the current store. It must reflect Endpoint and Link changes written through action dispatch. The daemon must not return stale startup context in place of the store-backed topology.

Endpoint health values are:

```text
healthy
degraded
blocked
unreachable
unknown
```

Link health is derived from source endpoint, target endpoint, target reachability, protocol family, auth mode, scope, and optional latency.

Relevant evidence:

```text
daemon_topology_reflects_endpoint_link_mutations
topology_is_rebuilt_from_store_after_actions
topology_reflects_endpoint_link_health
reconcile_tick_snapshot_uses_refreshed_store_state
```
