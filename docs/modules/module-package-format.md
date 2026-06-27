# Module Package Format

Document status: current implementation, package format v1.

## Format

`.ojosmod` is a zip package. Package format version `1` has this required structure:

```text
module.yaml
checksums.sha256
package.yaml
```

Optional metadata-only content may include:

```text
README.md
LICENSE
migrations/
assets/
frontend/
services/
tests/
```

`frontend/` and `services/` content is metadata only in v1. OJOS does not dynamically execute frontend bundles, service commands, hooks or scripts from a package.

`package.yaml` contains:

```yaml
package:
  format: ojosmod
  version: 1
  created_by: ojosctl
  signature: null
  signing_key_id: null
  trusted_publisher: null
```

## Required Verification

`ojosctl module verify <package.ojosmod>` checks:

- `module.yaml` exists and validates against `schema_version: 1`.
- `checksums.sha256` exists.
- `package.yaml` exists with `package.format=ojosmod` and `package.version=1`.
- Every file checksum matches.
- Every non-checksum file is listed in the checksum manifest.
- Path traversal is rejected.
- Absolute paths are rejected.
- Symlinks are rejected.
- `.env`, `.tmp`, `node_modules`, `frontend/dist`, `.git` and `target` entries are rejected.
- Hook/script/postinstall/preinstall/executable entry semantics are rejected by manifest and package validation.

## Signature Boundary

Package v1 verifies checksum integrity only. It does not prove publisher trust.

The following fields are reserved:

```text
signature
signing_key_id
trusted_publisher
```

Remote module market installation remains out of scope until package signature and publisher trust policy are implemented.

## Baseline Freeze

Package format is frozen as `.ojosmod` package version `1`. Future breaking package layout changes must use package format version `2`.
