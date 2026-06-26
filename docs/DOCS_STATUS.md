# 文档状态

> 文档状态：当前实现
> 适用范围：开发 / 部署 / 运维 / 架构设计 / 安全 / E2E 验收
> 最后更新：2026-06-26

本文档记录每篇正式文档的状态。状态只描述文档和对应能力的成熟度，不代表尚未执行的运行验收已经通过。

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
| development/backend-development.md | 当前实现 |
| development/temp-file-policy.md | 当前实现 |
| archive/legacy-docs/DOCS_MIGRATION.md | 已归档 |
| archive/legacy-docs/* | 已归档 |

## 状态解释

- 当前实现：文档描述当前仓库中已经实现并可静态验证的能力。
- 部分实现：主体链路存在，但仍有目标能力或运行验收未完成。
- 目标架构：设计方向，不应当理解为当前已上线能力。
- 需要运行验收：代码、配置或脚本存在，但必须在 Docker/Linux/多机环境中实际执行。
- 已归档：历史文档，仅供追溯，不可作为当前部署依据。

## 验收方式

维护者更新状态后，应同步更新 [文档索引](DOCS_INDEX.md)，并执行静态验证。

## 相关文档

- [文档首页](README.md)
- [工程验收总入口](e2e/e2e-engineering-acceptance.md)

## 剩余风险或外部环境要求

状态为“需要运行验收”的文档必须在具备 Docker daemon、Linux cgroup v2、nsjail 或多机 worker 条件时补充实际结果。
