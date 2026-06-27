# Module Contract v1

Module contract v1 is the stable compatibility starting point for OJOS modules. A module that stays inside this contract can contribute metadata, permissions, menus, frontend route metadata, gateway route metadata, services, workers, health checks, storage metadata, events, admin panel metadata and topology without changing Kernel, Gateway or Web Shell core logic.

## Compatibility Policy

- `schema_version: 1` is the compatibility start.
- Unknown top-level fields are rejected.
- Unknown `provides` fields are rejected in v1. This is intentionally strict until extension point governance is formalized.
- Dangerous unknown fields are rejected anywhere in the manifest.
- Adding a new optional field to v1 must be backward compatible and must not change existing semantics.
- Deleting or renaming a field requires `schema_version: 2`.
- Modules must not use manifest fields to execute code, fetch remote code or control host runtime.

Dangerous field names rejected anywhere include `secret`, `token`, `password`, `private_key`, `env`, `command`, `script`, `hook`, `image`, `mount`, `host_path`, `privileged`, `cap_add`, `postinstall`, `preinstall`, `remote_url`, `download_url` and `target_url`.

## Required Identity Fields

- `schema_version`: must be `1`.
- `id`: reverse-domain style lowercase module id such as `ojos.sample-hello`.
- `name`: display name.
- `version`: semver.
- `set`: module set/category.
- `kind`: `kernel`, `platform`, `feature`, `integration` or `metadata`.
- `status`: `builtin`, `external` or `demo`.
- `description`: optional, max 2000 characters.

## Compatibility And Dependencies

`compatibility.platform` and `compatibility.installer` declare minimum platform/installer constraints. `requires.modules` declares module dependencies with optional version constraints.

## Provides

`provides.permissions` declares permission keys.

`provides.roles` declares role metadata.

`provides.components` declares generic component metadata.

`provides.services` declares service lifecycle metadata. Metadata services use `lifecycle: metadata` and `trusted_runtime: metadata`; managed compose services must reference a trusted compose service name and cannot declare images, commands or mounts.

`provides.workers` declares worker lifecycle metadata with the same runtime safety rules.

`provides.frontend_routes` declares frontend route metadata. Web Shell does not dynamically import unknown component keys.

`provides.menus` declares menu metadata. Menu visibility is permission-aware and disabled menus are not active navigation.

`provides.gateway_routes` declares route prefix and `service_id`. It must not declare arbitrary `target_url`; Gateway resolves `service_id` through trusted configuration.

`provides.storage_buckets`, `health_checks`, `migrations`, `events`, `scheduled_jobs`, `admin_panels` and `topology.nodes/edges` are metadata extension points consumed by Runtime Snapshot and admin views.

## Package

`.ojosmod` v1 packages contain:

- `module.yaml`
- `checksums.sha256`
- `package.yaml`

Package v1 verifies checksum integrity only. Signature trust policy is reserved for a later trust release.

## Hotplug Level

Schema v1 supports L0 metadata hotplug, L1 Gateway route table contribution, and L2 controlled service plan metadata. It does not implement dynamic frontend bundles, hooks, remote module market or full hotplug automation.

## Version Freeze

`schema_version: 1` is frozen as the current compatibility starting point. Future breaking changes must use `schema_version: 2`; additive fields in v1 must be backward compatible and must not make old manifests unsafe or semantically different.

## Version Freeze

`schema_version: 1` is frozen as the current compatibility starting point. Future breaking changes must use `schema_version: 2`; additive fields in v1 must be backward compatible and must not make old manifests unsafe or semantically different.
