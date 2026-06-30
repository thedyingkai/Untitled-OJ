# Action Model

GUI, TUI, and backend entry points use the same action registry, form schema, plan schema, result schema, and error schema.

Formal actions come from `platform/schemas/orchestrator/actions.yaml` and are validated by `services/orchestrator/core/src/action.rs`.

Actions are organized by abstraction layer. Each formal layer must expose CRUD-style actions:

```text
release
host
service
endpoint
link
route
frontend
migration
permission
redis
storage
config
secret
topology
operation
log
diagnostic
```

Every layer has `create`, `list`, `get`, `update`, and `delete`. Domain-specific verbs such as `validate`, `install`, `apply`, `health.check`, `query`, or `export` are allowed only as extra actions on top of that CRUD base.

`service-name[*]` is not a formal table, entity, or action layer. It is only a query over currently running endpoints with the same service name. The daemon may keep legacy `/sets/{id}/expand` and `/sets/{id}/apply` HTTP routes as gone/compatibility responses, but `set.expand` and `set.apply` are not catalog actions.

Endpoint identity is always:

```text
ip:port:service-name
```

No `instance-id` is introduced.

## Execution Contract

`ActionRequest` values may be produced by GUI, TUI, or backend HTTP handlers, but operation plans must be produced by core. Entry layers must not assemble plans, mutate operation state machines, or bypass the core executor.

`services/orchestrator/core/src/dispatcher.rs` is the single action dispatcher. It reads the action schema, builds an `Operation`, writes the store when the action is supported, invokes fixed executor paths where available, and returns an `ActionDispatchResult`.

Every dispatch result has an explicit capability status:

```text
REAL          performed a real probe or real read and persisted observable results
STORE_BACKED  wrote Store, Operation, OperationLog, or view metadata without external execution
UNSUPPORTED   cannot currently perform the requested mutation and must not be shown as success
READONLY      computed or read data without mutating core objects
```

Unsupported catalog actions are never routed through a fake success path.

## Current Capability Matrix

| Layer | Representative actions | Status |
| --- | --- | --- |
| release | `release.create/list/get/update/delete`, `release.validate`, `release.install` | CRUD plus install path; install is store-backed |
| service | `service.create/list/get/update/delete`, `service.start/stop/restart/enable/disable`, `service.health.check` | CRUD is cataloged; lifecycle mutations remain unsupported until a safe driver binding exists |
| endpoint | `endpoint.create/list/get/update/delete`, `endpoint.health.check` | store-backed CRUD; health check can be real |
| link | `link.create/list/get/update/delete`, `link.health.check` | store-backed CRUD; health check can be real |
| topology | `topology.create/list/get/update/delete`, `topology.validate`, `topology.apply`, `topology.export` | read/validate/export supported; unsupported mutations are explicit |
| operation | `operation.create/list/get/update/delete`, `operation.confirm`, `operation.apply`, `operation.rollback`, `operation.cancel` | store-backed operation state machine |
| log | `log.create/list/get/update/delete`, `log.query` | log view CRUD and operation log queries |
| diagnostic | `diagnostic.create/list/get/update/delete`, `diagnostic.export` | store-backed reports and exports |
| host/route/frontend/migration/permission/redis/storage/config/secret | CRUD base | cataloged with explicit unsupported/read-only capability until backing stores are implemented |

Diagnostic reports include action capability evidence so `STORE_BACKED` and `UNSUPPORTED` paths are not confused with `REAL` execution.

## Backend API

The orchestrator backend converts HTTP requests into `ActionRequest` and calls the same dispatcher used by GUI and TUI.

Current write/read entry points are:

```text
POST /actions
POST /endpoints
PATCH /endpoints/{endpoint}
DELETE /endpoints/{endpoint}
POST /endpoints/{endpoint}/health
POST /endpoints/health
POST /links
PATCH /links/{source_endpoint}/{target_endpoint}
DELETE /links/{source_endpoint}/{target_endpoint}
POST /links/{source_endpoint}/{target_endpoint}/health
POST /links/health
POST /operations/plan
POST /operations/{operation_id}/confirm
POST /operations/{operation_id}/apply
POST /operations/{operation_id}/rollback
GET  /operations/{operation_id}/logs
POST /diagnostics
GET  /diagnostics/{report_id}
GET  /diagnostics/{report_id}.json
GET  /diagnostics/{report_id}.md
```

`GET /topology` is rebuilt from the current store state: services, endpoints, links, operations, log views, and diagnostic reports. Formal service-set persistence is not part of the store.

## Driver Boundary

Drivers accept only fixed actions. Arbitrary shell, arbitrary script paths, user-provided command strings, and remote root shells are outside the action model.

The current fixed drivers are:

```text
LocalProcessDriver
DockerComposeDriver
ExternalEndpointDriver
```

Driver capabilities are lower-level implementation details. The action console still reports unsupported service lifecycle commands as `UNSUPPORTED` until the project has a safe binding for starting, stopping, and deleting services.
