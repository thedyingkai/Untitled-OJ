# Module Authoring Guide

Start with `ojosctl module init`, then edit `module.yaml` within schema v1.

## Permissions

Declare permission keys under `provides.permissions`. Runtime Snapshot exposes enabled module permissions to the admin permission registry.

## Menus And Frontend Routes

Declare menu metadata under `provides.menus` and route metadata under `provides.frontend_routes`. Unknown `component_key` values are rendered by safe contribution views; Web Shell does not import dynamic JavaScript.

## Gateway Routes

Declare a prefix and `service_id`:

```yaml
gateway_routes:
  - prefix: /api/sample-hello
    service_id: sample-hello-api
    auth_mode: user
    enabled: false
```

The manifest cannot provide a URL. Gateway owns trusted upstream configuration and reserved prefix protection.

## Services And Workers

Metadata-only declarations are safe defaults:

```yaml
services:
  - id: sample-hello-metadata-service
    lifecycle: metadata
    trusted_runtime: metadata
```

Managed compose services require deploy/operator allowlisting. A module cannot make itself executable by declaring image, command or mount fields.

## Health And Topology

Declare health metadata under `provides.health_checks` and topology nodes/edges under `provides.topology`. Runtime Snapshot and Admin Topology render these automatically when the module is enabled.

## Lifecycle

Install writes registry metadata. Enable activates runtime contributions. Disable preserves registry data but removes active contributions. `include_disabled=true` is for admin inspection only.
