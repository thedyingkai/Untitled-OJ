# 文档索引

> 文档状态：当前实现
> 适用范围：开�?/ 部署 / 运维 / 架构设计 / 安全 / E2E 验收
> 最后更新：2026-06-27

本索引按主题列出 OJOS 当前正式文档。历史文档仅供追溯，不作为当前部署依据�?
## 架构

- [架构总览](architecture/overview.md)
- [Project Structure v2](architecture/project-structure-v2.md)
- [模块拓扑设计](architecture/module-topology.md)
- [服务拓扑](architecture/service-topology.md)
- [Worker Link 协议](architecture/worker-link-protocol.md)
- [Storage �?artifact 模型](architecture/storage-artifact-model.md)
- [权限模型](architecture/permission-model.md)
- [内部 HMAC](architecture/internal-auth.md)

## API

- [API 总览](api/README.md)
- [Auth API](api/auth-api.md)
- [Problem API](api/problem-api.md)
- [Judge API](api/judge-api.md)
- [Worker API](api/worker-api.md)
- [Admin API](api/admin-api.md)

## 前端

- [前端开发指南](development/frontend-development.md)
- [UI 风格指南](development/ui-style-guide.md)

## 模块系统

- [Kernel Overview](kernel/kernel-overview.md)
- [Kernel Installer](kernel/installer.md)
- [Kernel Module Runtime](kernel/module-runtime.md)
- [模块系统首页](modules/README.md)
- [模块契约](modules/module-contract.md)
- [模块清单](modules/module-manifest.md)
- [模块生命周期](modules/module-lifecycle.md)
- [模块安装器](modules/module-installer.md)
- [Judge Core 模块](modules/judge-core.md)
- [Contest 模块规划](modules/contest-planning.md)

## Judge

- [资源限制](judge/judge-resource-limits.md)
- [状态模型](judge/judge-status-model.md)
- [Worker 集群](judge/judge-worker-cluster.md)
- [语言运行时](judge/judge-language-runtime.md)
- [评测 E2E 用例](judge/judge-e2e-cases.md)

## 部署

- [Control Plane 部署](deploy/deploy-control-plane.md)
- [Worker Node 部署](deploy/deploy-worker-node.md)
- [Docker Compose 说明](deploy/docker-compose.md)
- [环境变量参考](deploy/env-reference.md)
- [生产加固](deploy/production-hardening.md)

## 运维

- [健康检查](operations/health-checks.md)
- [管理员操作](operations/admin-operations.md)
- [备份与保留](operations/backup-retention.md)
- [故障排查](operations/troubleshooting.md)

## E2E

- [工程验收总入口](e2e/e2e-engineering-acceptance.md)
- [Linux 运行验收](e2e/e2e-linux-runtime.md)
- [静态检查](e2e/e2e-static-checks.md)

## 安全

- [安全边界](security/security-boundary.md)
- [内部 HMAC](security/internal-hmac.md)
- [Worker Token](security/worker-token.md)
- [路径泄露防护](security/path-leak-prevention.md)
- [权限管理安全](security/permission-admin.md)
- [Module Installer Threat Model](security/module-installer-threat-model.md)

## 开�?
- [当前状态](development/current-state.md)
- [本地开发](development/local-development.md)
- [静态验证](development/static-verification.md)
- [编码规范](development/coding-standards.md)
- [前端开发指南](development/frontend-development.md)
- [UI 风格指南](development/ui-style-guide.md)
- [后端开发指南](development/backend-development.md)
- [临时文件隔离规则](development/temp-file-policy.md)

## 归档

- [历史文档目录](archive/legacy-docs/)
- [文档迁移记录](archive/legacy-docs/DOCS_MIGRATION.md)
- [Module Installer 仓库边界 ADR](architecture/adr/ADR-module-installer-repository-boundary.md)
- [Project Structure v2 ADR](architecture/adr/ADR-project-structure-v2-kernel-modules.md)
- [模块包格式](modules/module-package-format.md)

## Kernel Runtime Wiring v1 Documents

- `docs/kernel/module-runtime.md` describes Runtime Snapshot v1, include-disabled behavior, topology generation and L0/L1 hotplug status.
- `docs/api/admin-api.md` lists runtime snapshot, runtime routes and runtime reload admin APIs.
- `docs/modules/module-contract.md` describes manifest extension points consumed by Runtime Snapshot.
- `docs/development/frontend-development.md` documents Web Shell contribution rendering boundaries.
- `docs/development/backend-development.md` documents Gateway/runtime aggregation boundaries.

## Hotplug L1 Completion Documents

- `docs/kernel/module-runtime.md` now documents dynamic Gateway proxy, trusted service map, reserved prefixes, auth modes and route table reload semantics.
- `docs/modules/module-contract.md` documents `gateway_routes.service_id` and safe Web Shell contribution fallback.
- `docs/api/admin-api.md` documents runtime route table fields and dynamic proxy security boundaries.
- `docs/security/module-installer-threat-model.md` includes L1 dynamic proxy threats and mitigations.