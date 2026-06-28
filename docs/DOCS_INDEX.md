# OJOS 文档索引

本索引只指向当前维护中的正式文档。历史材料保留在 `docs/archive/`，规划材料保留在 `docs/features/`。

## 发布与门禁

- [v0.1.0 发布说明](release/v0.1.0-release-notes.md)
- [v0.1.0 发版清单](release/v0.1.0-ship-checklist.md)
- [v0.1.0 验收报告模板](release/v0.1.0-acceptance-report.md)
- [v0.1.0 已知限制](release/v0.1.0-known-limitations.md)
- [v0.1.0 发布产物](release/v0.1.0-artifacts.md)
- [Kernel 基线冻结](release/kernel-baseline-freeze.md)
- [Feature 前置门禁](release/pre-feature-gate.md)
- [验收矩阵](release/acceptance-matrix.md)
- [回归矩阵](release/regression-matrix.md)
- [版本与契约冻结](release/versioning.md)

## 功能规划

- [功能模块路线图](features/feature-module-roadmap.md)
- [第一个功能模块决策](features/first-feature-module-decision.md)
- [Contest Core 模块计划](features/contest-core-module-plan.md)
- [Contest Core 数据模型草案](features/contest-core-data-model.md)
- [Contest Core API 草案](features/contest-core-api.md)
- [Contest Core 前端草案](features/contest-core-frontend.md)
- [Contest Core Runtime 与拓扑草案](features/contest-core-runtime.md)
- [Contest Core 验收矩阵](features/contest-core-acceptance.md)
- [Contest Core 风险评估](features/contest-core-risk-review.md)
- [Contest Core 实现前门禁](features/contest-core-pre-implementation-gate.md)

## Kernel

- [Kernel 概览](kernel/kernel-overview.md)
- [Kernel Installer](kernel/installer.md)
- [Kernel Module Runtime](kernel/module-runtime.md)

## 模块系统

- [模块系统入口](modules/README.md)
- [模块契约](modules/module-contract.md)
- [Module Contract v1](modules/module-contract-v1.md)
- [Module Schema v1](modules/module-schema-v1.yaml)
- [Module SDK](modules/module-sdk.md)
- [模块编写指南](modules/module-authoring-guide.md)
- [模块测试指南](modules/module-testing-guide.md)
- [无需改 Kernel 的扩展证明](modules/no-kernel-change-extension-proof.md)
- [模块生命周期](modules/module-lifecycle.md)
- [模块安装器](modules/module-installer.md)
- [模块包格式](modules/module-package-format.md)
- [Judge Core 模块](modules/judge-core.md)
- [Judge Core readiness](modules/judge-core-readiness.md)
- [Contest 规划归档入口](modules/contest-planning.md)

## 架构

- [架构概览](architecture/overview.md)
- [Project Structure v2](architecture/project-structure-v2.md)
- [模块拓扑](architecture/module-topology.md)
- [服务拓扑](architecture/service-topology.md)
- [权限模型](architecture/permission-model.md)
- [内部认证](architecture/internal-auth.md)
- [存储与制品模型](architecture/storage-artifact-model.md)
- [Worker Link 协议](architecture/worker-link-protocol.md)

## API

- [API 索引](api/README.md)
- [Admin API](api/admin-api.md)
- [Auth API](api/auth-api.md)
- [Problem API](api/problem-api.md)
- [Judge API](api/judge-api.md)
- [Worker API](api/worker-api.md)

## 安全

- [安全边界](security/security-boundary.md)
- [Kernel 安全复核](security/kernel-security-review.md)
- [Module Installer 威胁模型](security/module-installer-threat-model.md)
- [内部 HMAC](security/internal-hmac.md)
- [路径泄露防护](security/path-leak-prevention.md)
- [权限管理安全](security/permission-admin.md)
- [Worker Token](security/worker-token.md)

## 开发

- [当前状态](development/current-state.md)
- [本地开发](development/local-development.md)
- [后端开发](development/backend-development.md)
- [前端开发](development/frontend-development.md)
- [静态验证](development/static-verification.md)
- [编码规范](development/coding-standards.md)
- [临时文件规则](development/temp-file-policy.md)
- [UI 风格指南](development/ui-style-guide.md)

## E2E

- [工程验收](e2e/e2e-engineering-acceptance.md)
- [Linux Runtime 验收](e2e/e2e-linux-runtime.md)
- [静态检查](e2e/e2e-static-checks.md)

## Judge

- [Judge E2E 用例](judge/judge-e2e-cases.md)
- [语言运行时](judge/judge-language-runtime.md)
- [资源限制](judge/judge-resource-limits.md)
- [评测状态模型](judge/judge-status-model.md)
- [Judge Worker 集群](judge/judge-worker-cluster.md)

## 部署

- [Control Plane 部署](deploy/deploy-control-plane.md)
- [Worker Node 部署](deploy/deploy-worker-node.md)
- [Docker Compose](deploy/docker-compose.md)
- [环境变量参考](deploy/env-reference.md)
- [生产加固](deploy/production-hardening.md)

## 运维

- [管理员运维](operations/admin-operations.md)
- [备份保留](operations/backup-retention.md)
- [健康检查](operations/health-checks.md)
- [故障排查](operations/troubleshooting.md)

## 脚本

- `scripts/acceptance-kernel.ps1`：统一本地 Kernel 基线验收。
- `scripts/verify-static.ps1`：静态构建、测试与安全扫描。
- `scripts/e2e-api.ps1`：Docker control plane API e2e。
- `scripts/e2e-module-compat.ps1`：Module SDK compatibility harness。
- `scripts/build-release-artifacts.ps1`：v0.1.0 发布产物构建。
