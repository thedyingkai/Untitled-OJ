# ADR: Module Installer Repository Boundary

Status: Accepted

Date: 2026-06-27

## Context

The Module Installer is a core OJOS system foundation. It owns manifest validation, package verification, dependency planning, module lifecycle operations, operation locks, and audit/history records. It must integrate with the current Control Plane database, Gateway Admin API, frontend module management pages, Docker Compose deployment, and the Module Registry v0 schema.

The boundary must support fast v0 iteration without making the installer impossible to split into a dedicated repository later.

## Decision

OJOS will not split the Module Installer into a separate repository immediately. The installer will be implemented inside the OJOS monorepo as an independent Rust workspace:

```text
crates/module-installer-core/
services/module-installer/
tools/ojosctl/
```

This is option C: monorepo placement with independent-repository boundaries.

The installer code must not depend on Go service internals or frontend code. Its contracts are limited to:

- PostgreSQL schema and transactions
- Internal HTTP API
- Module manifest schema
- Module package format
- Stable JSON request/response models

## Options

### Option A: Keep Installer Directly In The OJOS Monorepo

Paths:

```text
services/module-installer/
crates/module-installer-core/
tools/ojosctl/
```

Pros:

- Simplest integration with the current DB schema, Gateway, frontend, and Compose stack.
- One CI and version stream.
- No separate publishing or version pinning process.
- Best fit for rapid v0 to v1 iteration.

Cons:

- If the installer becomes reusable outside OJOS, it will inherit main-repo coupling.
- The installer foundation is less visibly independent.

### Option B: Split To A Separate Repository Now

Candidate names:

```text
ojos-installer
ojos-module-installer
ojos-module-runtime
```

Pros:

- Clearest architectural boundary.
- Can independently publish the CLI, library, and service.
- Better long-term reuse story.

Cons:

- Current schema and manifest contracts are still changing.
- Integration cost rises immediately.
- CI, release, version pinning, and cross-repo compatibility must be solved before v0.
- It risks slowing the Control Plane v0 path.

### Option C: Independent Rust Workspace Inside The Monorepo

Pros:

- Keeps v0 integration fast.
- Gives the installer a real library/service/CLI boundary.
- Keeps Rust APIs clear and testable.
- Allows future migration of the workspace to an independent repository.
- Avoids direct dependencies on Go or frontend implementation details.

Cons:

- Requires discipline to keep contracts explicit.
- CI still runs in the main repository until split.

## Consequences

The installer workspace is designed so it can later be moved to a separate repository with minimal code changes. All direct integration points must be documented and tested:

- DB tables and migrations
- Internal API endpoints
- Manifest schema
- Package format
- Gateway Admin API mapping
- Frontend API types

The Gateway remains the only public entry point. It performs JWT authentication and admin/system.admin authorization, then calls the internal Rust installer service. The installer service is not exposed to the host network.

## Future Split Triggers

The installer should be split into a dedicated repository when one or more of these conditions become true:

- The installer is reused by multiple OJOS deployments or external systems.
- The module package format is stable.
- The installer CLI needs independent releases.
- The installer service needs an independent version lifecycle.
- Main repository CI is materially slowed by installer build/test cost.
- Manifest schema and installer API compatibility need formal version pinning.

## Boundaries For v0

v0 supports local manifests and local `.ojosmod` packages. It does not support a remote marketplace, untrusted remote install, dynamic frontend bundles, or executable install hooks.

v0 performs checksum integrity verification. Signature fields are reserved in the schema, but signature validation and trust policy are deferred to v1.

Kernel modules and `ojos.judge-core` are protected from disable/uninstall apply operations. Demo modules may be used for lifecycle acceptance.
