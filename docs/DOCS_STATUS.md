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
| deploy/deploy-worker-node.md | 需要真实多机验收 |
| deploy/docker-compose.md | 当前实现 |
| deploy/env-reference.md | 当前实现 |
| deploy/production-hardening.md | 当前实现 |
| judge/judge-resource-limits.md | 当前实现，WSL2 Linux 已验收 |
| judge/judge-status-model.md | 当前实现 |
| judge/judge-worker-cluster.md | 部分实现，本机双 worker 已验收，真实多机待验收 |
| judge/judge-language-runtime.md | 需要运行验收 |
| judge/judge-e2e-cases.md | 当前实现，WSL2 Linux 已验收 |
| security/security-boundary.md | 当前实现 |
| security/internal-hmac.md | 当前实现 |
| security/worker-token.md | 当前实现 |
| security/path-leak-prevention.md | 当前实现 |
| security/permission-admin.md | 当前实现 |
| security/module-installer-threat-model.md | 当前实现，Installer hardening 威胁模型 |
| e2e/e2e-engineering-acceptance.md | 当前实现 |
| e2e/e2e-linux-runtime.md | 当前实现，WSL2 Linux 已验收，真实多机待补充 |
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
- 需要真实多机验收：本机或单机模拟不足以证明独立 worker node、跨主机网络和故障恢复能力。
- WSL2 Linux 环境已验收：已在 WSL2 Linux + Docker + cgroup v2 + nsjail 环境执行通过，但不等同于真实多机生产验收。
- WSL2 本机双 worker 已验收：已在同一台 WSL2 Linux 主机用两个 worker 实例完成并发、lease 和恢复验收，但不等同于第二台真实 worker node 验收。
- 已归档：历史文档，仅供追溯。

当前 A/Judge Core 已通过 WSL2 Linux 本机运行验收，但真实第二台 worker node、跨主机网络抖动、断网恢复、时钟漂移和长时间 soak test 仍未执行，因此不得标记为 GA。

## 验收方式

维护文档状态后，应同步更新 [文档索引](DOCS_INDEX.md)，并执行静态验证。
## 2026-06-27 Module Installer 文档状态

| 文档 | 状态 |
| --- | --- |
| architecture/adr/ADR-module-installer-repository-boundary.md | 当前实现，采用 monorepo 内独立 Rust workspace |
| modules/module-installer.md | 当前实现，v0 本地 manifest / package / metadata lifecycle |
| modules/module-manifest.md | 当前实现，schema_version 1 |
| modules/module-lifecycle.md | 当前实现，v0 metadata lifecycle |
| modules/module-contract.md | 当前实现 |
| modules/module-package-format.md | 当前实现，checksum integrity；signature v1 |

边界：Installer v0 不支持远程市场、不执行不可信脚本、不加载动态 bundle、不宣称独立仓库发布完成。

# Module Installer Hardening Status

`services/module-installer` 已改为多阶段构建：builder 使用 `rust:1.89-bookworm`，最终 runtime 使用 `debian:bookworm-slim`，并以非 root 用户运行。Compose 对 `module-installer` 启用 `read_only: true`、`no-new-privileges:true`、`cap_drop: ALL`、`/tmp` tmpfs 和只读 `modules/` 挂载，不发布宿主机端口。

包格式新增 `package.yaml` metadata；v0 仍只做 checksum integrity，不提供 publisher trust。signature / trust policy、distroless runtime、完整通用 rollback/uninstall apply 留到 v1 或后续 hardening。
