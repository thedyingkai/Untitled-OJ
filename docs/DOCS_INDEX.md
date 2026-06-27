# OJOS Documentation Index

This index points to the current canonical docs. Historical docs remain under `docs/archive/`.

## Release And Gates

- [Kernel Baseline Freeze](release/kernel-baseline-freeze.md)
- [Pre-Feature Gate](release/pre-feature-gate.md)
- [Acceptance Matrix](release/acceptance-matrix.md)
- [Regression Matrix](release/regression-matrix.md)
- [Versioning And Contract Freeze](release/versioning.md)

## Feature Planning

- [Feature Module Roadmap](features/feature-module-roadmap.md)
- [First Feature Module Decision](features/first-feature-module-decision.md)
- [Contest Core Module Plan](features/contest-core-module-plan.md)
- [Contest Core Data Model Draft](features/contest-core-data-model.md)
- [Contest Core API Draft](features/contest-core-api.md)
- [Contest Core Frontend Draft](features/contest-core-frontend.md)
- [Contest Core Runtime And Topology Draft](features/contest-core-runtime.md)
- [Contest Core Acceptance Matrix](features/contest-core-acceptance.md)
- [Contest Core Risk Review](features/contest-core-risk-review.md)
- [Contest Core Pre-Implementation Gate](features/contest-core-pre-implementation-gate.md)

## Kernel

- [Kernel Overview](kernel/kernel-overview.md)
- [Kernel Installer](kernel/installer.md)
- [Kernel Module Runtime](kernel/module-runtime.md)

## Modules

- [Module System](modules/README.md)
- [Module Contract](modules/module-contract.md)
- [Module Contract v1](modules/module-contract-v1.md)
- [Module Schema v1](modules/module-schema-v1.yaml)
- [Module SDK](modules/module-sdk.md)
- [Module Authoring Guide](modules/module-authoring-guide.md)
- [Module Testing Guide](modules/module-testing-guide.md)
- [No Kernel Change Extension Proof](modules/no-kernel-change-extension-proof.md)
- [Module Lifecycle](modules/module-lifecycle.md)
- [Module Installer](modules/module-installer.md)
- [Module Package Format](modules/module-package-format.md)
- [Judge Core](modules/judge-core.md)
- [Judge Core Readiness](modules/judge-core-readiness.md)
- [Contest Planning](modules/contest-planning.md)

## Architecture

- [Architecture Overview](architecture/overview.md)
- [Project Structure v2](architecture/project-structure-v2.md)
- [Module Topology](architecture/module-topology.md)
- [Service Topology](architecture/service-topology.md)
- [Permission Model](architecture/permission-model.md)
- [Internal Auth](architecture/internal-auth.md)
- [Storage Artifact Model](architecture/storage-artifact-model.md)
- [Worker Link Protocol](architecture/worker-link-protocol.md)

## API

- [API Index](api/README.md)
- [Admin API](api/admin-api.md)
- [Auth API](api/auth-api.md)
- [Problem API](api/problem-api.md)
- [Judge API](api/judge-api.md)
- [Worker API](api/worker-api.md)

## Security

- [Security Boundary](security/security-boundary.md)
- [Kernel Security Review](security/kernel-security-review.md)
- [Module Installer Threat Model](security/module-installer-threat-model.md)
- [Internal HMAC](security/internal-hmac.md)
- [Path Leak Prevention](security/path-leak-prevention.md)
- [Permission Admin](security/permission-admin.md)
- [Worker Token](security/worker-token.md)

## Development

- [Current State](development/current-state.md)
- [Local Development](development/local-development.md)
- [Backend Development](development/backend-development.md)
- [Frontend Development](development/frontend-development.md)
- [Static Verification](development/static-verification.md)
- [Coding Standards](development/coding-standards.md)
- [Temporary File Policy](development/temp-file-policy.md)
- [UI Style Guide](development/ui-style-guide.md)

## E2E

- [Engineering Acceptance](e2e/e2e-engineering-acceptance.md)
- [Linux Runtime](e2e/e2e-linux-runtime.md)
- [Static Checks](e2e/e2e-static-checks.md)

## Judge

- [Judge E2E Cases](judge/judge-e2e-cases.md)
- [Judge Language Runtime](judge/judge-language-runtime.md)
- [Judge Resource Limits](judge/judge-resource-limits.md)
- [Judge Status Model](judge/judge-status-model.md)
- [Judge Worker Cluster](judge/judge-worker-cluster.md)

## Deployment

- [Control Plane Deployment](deploy/deploy-control-plane.md)
- [Worker Node Deployment](deploy/deploy-worker-node.md)
- [Docker Compose](deploy/docker-compose.md)
- [Environment Reference](deploy/env-reference.md)
- [Production Hardening](deploy/production-hardening.md)

## Operations

- [Admin Operations](operations/admin-operations.md)
- [Backup Retention](operations/backup-retention.md)
- [Health Checks](operations/health-checks.md)
- [Troubleshooting](operations/troubleshooting.md)

## Scripts

- `scripts/acceptance-kernel.ps1`: unified local kernel baseline acceptance.
- `scripts/verify-static.ps1`: static build/test/security verification.
- `scripts/e2e-api.ps1`: Docker control-plane API e2e.
- `scripts/e2e-module-compat.ps1`: Module SDK compatibility harness.
