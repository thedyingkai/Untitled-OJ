# 文档索引

> 文档状态：当前实现
> 适用范围：开发 / 部署 / 运维 / 架构设计 / 安全 / E2E 验收
> 最后更新：2026-06-26

本文档按主题列出 OJOS 当前文档。所有正式文档都应能从 [文档首页](README.md) 或本文档进入。

## 架构

- [架构总览](architecture/overview.md)
- [模块拓扑设计](architecture/module-topology.md)
- [服务拓扑](architecture/service-topology.md)
- [Worker Link 协议](architecture/worker-link-protocol.md)
- [Storage 与 artifact 模型](architecture/storage-artifact-model.md)
- [权限模型](architecture/permission-model.md)
- [内部 HMAC](architecture/internal-auth.md)

## 部署

- [Control Plane 部署](deploy/deploy-control-plane.md)
- [Worker Node 部署](deploy/deploy-worker-node.md)
- [Docker Compose 说明](deploy/docker-compose.md)
- [环境变量参考](deploy/env-reference.md)
- [生产加固](deploy/production-hardening.md)

## Judge

- [资源限制](judge/judge-resource-limits.md)
- [状态模型](judge/judge-status-model.md)
- [Worker 集群](judge/judge-worker-cluster.md)
- [语言运行时](judge/judge-language-runtime.md)
- [评测 E2E 用例](judge/judge-e2e-cases.md)

## 安全

- [安全边界](security/security-boundary.md)
- [内部 HMAC](security/internal-hmac.md)
- [Worker Token](security/worker-token.md)
- [路径泄露防护](security/path-leak-prevention.md)
- [权限管理安全](security/permission-admin.md)

## E2E

- [工程验收总入口](e2e/e2e-engineering-acceptance.md)
- [Linux 运行验收](e2e/e2e-linux-runtime.md)
- [静态检查](e2e/e2e-static-checks.md)

## API

- [API 总览](api/README.md)
- [Auth API](api/auth-api.md)
- [Problem API](api/problem-api.md)
- [Judge API](api/judge-api.md)
- [Worker API](api/worker-api.md)
- [Admin API](api/admin-api.md)

## 模块系统

- [模块系统首页](modules/README.md)
- [模块契约](modules/module-contract.md)
- [模块清单](modules/module-manifest.md)
- [模块生命周期](modules/module-lifecycle.md)
- [模块安装器](modules/module-installer.md)
- [Judge Core 模块](modules/judge-core.md)
- [Contest 模块规划](modules/contest-planning.md)

## 运维

- [健康检查](operations/health-checks.md)
- [管理员操作](operations/admin-operations.md)
- [备份与保留](operations/backup-retention.md)
- [故障排查](operations/troubleshooting.md)

## 开发

- [当前状态](development/current-state.md)
- [本地开发](development/local-development.md)
- [静态验证](development/static-verification.md)
- [编码规范](development/coding-standards.md)
- [前端开发](development/frontend-development.md)
- [后端开发](development/backend-development.md)
- [临时文件隔离规则](development/temp-file-policy.md)

## 归档

- [历史文档目录](archive/legacy-docs/)
- [文档迁移记录](archive/legacy-docs/DOCS_MIGRATION.md)

## 验收方式

执行文档链接检查和静态验证；归档文档只保留历史参考，不作为部署依据。

## 相关文档

- [文档状态](DOCS_STATUS.md)

## 剩余风险或外部环境要求

需要 Docker daemon、Linux cgroup v2、nsjail 或多机 worker 的验收，必须在对应环境中单独记录结果。
