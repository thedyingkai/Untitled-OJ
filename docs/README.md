# OJOS 文档中心

> 文档状态：当前实现
> 适用范围：开发 / 部署 / 运维 / 架构设计 / 安全 / E2E 验收
> 最后更新：2026-06-27

OJOS 当前处于 Core Judge Platform 阶段，已具备分布式 OJ 的主体链路：Gateway、Auth、Problem API、Judge API、前端、PostgreSQL、Redis signal stream、Worker Link 和 Rust judge-worker。

目标架构将继续扩展为 Kernel + Core + 可安装模块的 OJ Operating System。标记为“目标架构”的文档只代表后续设计方向，不等于当前已经上线。

## 推荐阅读顺序

1. [当前状态](development/current-state.md)
2. [架构总览](architecture/overview.md)
3. [服务拓扑](architecture/service-topology.md)
4. [API 文档总览](api/README.md)
5. [前端开发指南](development/frontend-development.md)
6. [UI 风格指南](development/ui-style-guide.md)
7. [静态验证](development/static-verification.md)
8. [工程验收总入口](e2e/e2e-engineering-acceptance.md)

## 架构

- [架构总览](architecture/overview.md)
- [模块拓扑设计](architecture/module-topology.md)
- [服务拓扑](architecture/service-topology.md)
- [Worker Link 协议](architecture/worker-link-protocol.md)
- [Storage 与 artifact 模型](architecture/storage-artifact-model.md)
- [权限模型](architecture/permission-model.md)
- [内部 HMAC](architecture/internal-auth.md)

## API

- [API 文档总览](api/README.md)
- [Auth API](api/auth-api.md)
- [Problem API](api/problem-api.md)
- [Judge API](api/judge-api.md)
- [Worker API](api/worker-api.md)
- [Admin API](api/admin-api.md)

## 前端

- [前端开发指南](development/frontend-development.md)
- [UI 风格指南](development/ui-style-guide.md)

## 模块系统

- [模块系统首页](modules/README.md)
- [模块契约](modules/module-contract.md)
- [模块清单](modules/module-manifest.md)
- [模块生命周期](modules/module-lifecycle.md)
- [模块安装器](modules/module-installer.md)
- [Judge Core 模块](modules/judge-core.md)
- [Contest 模块规划](modules/contest-planning.md)

## Judge Core

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

## E2E 验收

- [工程验收总入口](e2e/e2e-engineering-acceptance.md)
- [Linux 运行验收](e2e/e2e-linux-runtime.md)
- [静态检查](e2e/e2e-static-checks.md)

## 安全

- [安全边界](security/security-boundary.md)
- [内部 HMAC](security/internal-hmac.md)
- [Worker Token](security/worker-token.md)
- [路径泄露防护](security/path-leak-prevention.md)
- [权限管理安全](security/permission-admin.md)

## 文档地图

- [文档索引](DOCS_INDEX.md)
- [文档状态](DOCS_STATUS.md)
- [历史文档目录](archive/legacy-docs/)

## 验收方式

修改文档或前端后至少执行：

```powershell
powershell -NoProfile -File scripts\verify-static.ps1 -SkipDockerBuild
```

涉及前端时还应执行：

```powershell
cd frontend
npm run build
cd ..
```
