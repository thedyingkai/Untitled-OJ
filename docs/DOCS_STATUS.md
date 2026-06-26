# 文档状态

> 文档状态：当前实现
> 适用范围：开发 / 部署 / 运维 / 架构设计 / 安全 / E2E 验收
> 最后更新：2026-06-27

本文记录正式文档的状态。状态描述文档与能力成熟度，不代表尚未执行的运行时验收已经通过。

| 文档 | 状态 |
| --- | --- |
| README.md | 当前实现 |
| DOCS_INDEX.md | 当前实现 |
| DOCS_STATUS.md | 当前实现 |
| architecture/overview.md | 部分实现 |
| architecture/module-topology.md | 部分实现 |
| architecture/service-topology.md | 当前实现 |
| architecture/worker-link-protocol.md | 部分实现 |
| architecture/storage-artifact-model.md | 部分实现 |
| architecture/permission-model.md | 当前实现 |
| architecture/internal-auth.md | 当前实现 |
| deploy/deploy-control-plane.md | 当前实现 |
| deploy/deploy-worker-node.md | 需要运行验收 |
| deploy/docker-compose.md | 当前实现 |
| deploy/env-reference.md | 当前实现 |
| deploy/production-hardening.md | 当前实现 |
| judge/judge-resource-limits.md | 需要运行验收 |
| judge/judge-status-model.md | 当前实现 |
| judge/judge-worker-cluster.md | 需要运行验收 |
| judge/judge-language-runtime.md | 需要运行验收 |
| judge/judge-e2e-cases.md | 需要运行验收 |
| security/security-boundary.md | 当前实现 |
| security/internal-hmac.md | 当前实现 |
| security/worker-token.md | 当前实现 |
| security/path-leak-prevention.md | 当前实现 |
| security/permission-admin.md | 当前实现 |
| e2e/e2e-engineering-acceptance.md | 当前实现 |
| e2e/e2e-linux-runtime.md | 需要运行验收 |
| e2e/e2e-static-checks.md | 当前实现 |
| api/README.md | 当前实现 |
| api/auth-api.md | 当前实现 |
| api/problem-api.md | 当前实现 |
| api/judge-api.md | 当前实现 |
| api/worker-api.md | 部分实现 |
| api/admin-api.md | 当前实现 |
| modules/README.md | 部分实现 |
| modules/module-contract.md | 部分实现 |
| modules/module-manifest.md | 部分实现 |
| modules/module-lifecycle.md | 部分实现 |
| modules/module-installer.md | 目标架构 |
| modules/judge-core.md | 部分实现 |
| modules/contest-planning.md | 目标架构 |
| operations/health-checks.md | 当前实现 |
| operations/admin-operations.md | 当前实现 |
| operations/backup-retention.md | 部分实现 |
| operations/troubleshooting.md | 当前实现 |
| development/current-state.md | 当前实现 |
| development/local-development.md | 当前实现 |
| development/static-verification.md | 当前实现 |
| development/coding-standards.md | 当前实现 |
| development/frontend-development.md | 当前实现 |
| development/ui-style-guide.md | 当前实现 |
| development/backend-development.md | 当前实现 |
| development/temp-file-policy.md | 当前实现 |
| archive/legacy-docs/* | 已归档 |

## 状态解释

- 当前实现：描述当前仓库中已实现并可静态验证的能力。
- 部分实现：主体链路存在，但仍有目标能力或运行验收未完成。
- 目标架构：设计方向，不应理解为当前已上线能力。
- 需要运行验收：代码、配置或脚本存在，但必须在 Docker/Linux/多机环境中实际执行。
- 已归档：历史文档，仅供追溯。

## 验收方式

维护文档状态后，应同步更新 [文档索引](DOCS_INDEX.md)，并执行静态验证。
