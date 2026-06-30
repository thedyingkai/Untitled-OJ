# Orchestrator Requirements

The product name is OJOS Orchestrator.

Orchestrator manages these formal layers:

```text
ServiceRelease
Host
Service
Endpoint
Link
Route
FrontendEntry
Migration
Permission
RedisResource
StorageResource
Config
Secret
Topology
Operation
LogView
DiagnosticReport
```

Every formal action layer exposes CRUD-style actions. Extra verbs such as `validate`, `install`, `apply`, `health.check`, `query`, and `export` are layer-specific additions, not substitutes for CRUD coverage.

The basic install unit is a service release. A service release may carry backend, frontend, migration, permission, route, Redis, storage, config, secret, dependency, and observability declarations.

Endpoint identity is always `ip:port:service-name`. `instance-id` is not part of the model.

`service-name[*]` is a derived query over running endpoints with the same service name. Local deployment templates may be shown as read-only helper material, but they are not formal store objects and do not have formal actions.

The formal entry points are Orchestrator GUI, Orchestrator TUI, and Orchestrator daemon. All three must call the same `services/orchestrator/core` and `platform/schemas/orchestrator` contracts. Differences are interaction or transport shape only, not capability.

Orchestrator does not implement OJ business features such as problems, submissions, users, contests, training, clarifications, printing, ranking, or site administration. Those features belong to managed services.
