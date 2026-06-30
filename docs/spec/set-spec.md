# Deployment Template Spec

Deployment Template is a read-only local deployment helper. It is not a runtime
object, database table, formal action layer, or business API. It only describes
recommended services, default endpoints, default links, install order, start
order, and placement policy.

## template.yaml

```yaml
schema_version: 1
id:
name:
description:

scenario:
  type:
  recommended_for:

services:
  - id:
    required:
    count:
    placement:
    config:

default_endpoints:
  - service:
    port:
    protocol:
    expose:

default_links:
  - from:
    to:
    protocol:
    auth_mode:
    scope:
    required:

policies:
  placement:
  security:
  network:
  health:

operations:
  install_order:
  start_order:
  stop_order:

notes:
```

## Current Local Templates

The repository keeps five local templates:

```text
single-node-oj
distributed-oj
judge-worker-node
course-judge
service-development
```

Deployment Template does not introduce host objects, device objects, install
instance objects, package objects, or service-set persistence. Placement is
expressed by `placement` policy and runtime Endpoint identity
`ip:port:service-name`.

Every referenced Service must exist under `services/*/service.yaml`.
`default_endpoints` and `default_links` may only reference services listed in
the same local template. If a required link target is provided by another
template or by an external endpoint, the template must declare that in
`policies.network.required_external_links`.

At runtime, Orchestrator creates real Links from concrete Endpoints. A
`service-name[*]` value is always derived by querying running Endpoints with the
same service name; it is not loaded from a template and is not persisted.
