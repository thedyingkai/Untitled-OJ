# Versioning And Contract Freeze

Date: 2026-06-27

## Frozen Versions

| Contract | Current version | Compatibility rule |
| --- | --- | --- |
| Module manifest schema | `schema_version: 1` | Breaking manifest changes require `schema_version: 2` |
| Runtime Snapshot | `version: "1"` | Breaking response changes require snapshot version `2` |
| `.ojosmod` package format | `package.version: 1` | Breaking package layout changes require package format version `2` |

## Manifest Compatibility

All checked-in module manifests must validate against schema v1:

- `modules/judge-core/module.yaml`
- `modules/demo-module/module.yaml`
- `modules/sample-hello/module.yaml`

Schema v1 rejects unknown top-level fields and unknown `provides` fields. New optional fields must be backward compatible and must not change existing semantics. Removing or renaming a field is a breaking change.

## Runtime Snapshot Compatibility

Runtime Snapshot v1 is the source of truth for active module contributions, route metadata, menu metadata, permissions, topology, services and workers. Admin `include_disabled=true` can reveal disabled registry contributions for inspection.

Breaking response changes require a new snapshot version and a compatibility adapter for existing Web Shell/admin clients.

## Package Compatibility

Package format v1 is a zip-based `.ojosmod` package with `module.yaml`, `checksums.sha256` and `package.yaml`.

Signature and publisher trust fields are reserved and may be null in v1. Checksum integrity does not prove publisher trust. Remote market installation remains out of scope until signature/trust policy is implemented.
