# Architecture Docs

The current architecture is service-release-first.

Formal Orchestrator objects are:

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

The runtime endpoint identity is `ip:port:service-name`.

`service-name[*]` is a derived query over running endpoints. Local deployment templates are read-only helpers and are not formal runtime objects.

Gateway and Gateway frontend are services, not the Orchestrator control plane. GUI and TUI are formal management entry points and must stay capability-equivalent through core.
