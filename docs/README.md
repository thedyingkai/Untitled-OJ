# OJOS 文档中心

> 文档状态：当前实现
> 适用范围：开发 / 部署 / 运维 / 架构设计 / 安全 / E2E 验收
> 最后更新：2026-06-26

本文档是 OJOS 项目的文档总入口。OJOS 当前处于 Core Judge Platform 阶段：已经具备分布式 OJ 的主体链路，包括 Gateway、Auth、Problem API、Judge API、前端、PostgreSQL、Redis signal stream、Worker Link 和 Rust judge-worker。

目标架构会继续扩展为 Kernel + Core + 可安装模块的 OJ Operating System。凡是标记为“目标架构”的文档，只代表后续设计方向，不等于当前已经上线。

## 推荐阅读顺序

1. 阅读 [当前状态](development/current-state.md)，先确认已经实现和仍需运行验收的范围。
2. 阅读 [架构总览](architecture/overview.md) 与 [服务拓扑](architecture/service-topology.md)。
3. 阅读 [Worker Link 协议](architecture/worker-link-protocol.md) 和 [Judge Worker 集群](judge/judge-worker-cluster.md)。
4. 按 [静态验证](development/static-verification.md) 执行本机可跑的构建和扫描。
5. 按 [Control Plane 部署](deploy/deploy-control-plane.md) 和 [Worker Node 部署](deploy/deploy-worker-node.md) 做运行环境准备。
6. 按 [端到端工程验收](e2e/e2e-engineering-acceptance.md) 记录实际执行结果。

## 当前已实现内容

- Gateway 作为唯一公开 API 入口。
- Auth 提供登录、注册、当前用户和权限管理 API。
- Problem API 提供题目列表、详情、CRUD 和题目包校验。
- Judge API 提交、列表、详情、case 结果、Worker Link 和管理 API。
- 前端页面覆盖用户、题目、提交、权限、健康检查和评测集群管理。
- PostgreSQL 作为事实源，Redis Streams 作为有界 signal history。
- Worker 通过 Worker Link 拉任务、下载 artifact、上传结果。

## 仍需运行验收的内容

以下内容不能在普通 Windows 静态环境中判定为通过：

- Docker daemon 下的镜像构建。
- Linux cgroup v2 与 nsjail 的资源限制真实执行。
- 两台独立 worker 机器的并发评测。
- cpp17、c11、python3、java17 的 AC/WA/CE/RE/TLE/MLE/OLE 全矩阵运行。
- worker 崩溃后的 lease 恢复与旧 lease result 拒绝。

## 架构文档

- [架构总览](architecture/overview.md)
- [模块拓扑设计](architecture/module-topology.md)
- [服务拓扑](architecture/service-topology.md)
- [Worker Link 协议](architecture/worker-link-protocol.md)
- [Storage 与 artifact 模型](architecture/storage-artifact-model.md)
- [权限模型](architecture/permission-model.md)
- [内部 HMAC](architecture/internal-auth.md)

## 模块系统

模块系统当前已经实现 Module Registry v0：可以登记 Kernel 内置模块和 `ojos.judge-core`，并通过只读 API 与后台页面展示模块集合、依赖、组件和安装状态。installer v0、动态启用/禁用、升级回滚和 B Contest 仍属于后续目标架构，不能写成已上线能力。

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

## 安全

- [安全边界](security/security-boundary.md)
- [内部 HMAC](security/internal-hmac.md)
- [Worker Token](security/worker-token.md)
- [路径泄露防护](security/path-leak-prevention.md)
- [权限管理安全](security/permission-admin.md)

## 运维

- [健康检查](operations/health-checks.md)
- [管理员操作](operations/admin-operations.md)
- [备份与保留](operations/backup-retention.md)
- [故障排查](operations/troubleshooting.md)

## E2E 验收

- [工程验收总入口](e2e/e2e-engineering-acceptance.md)
- [Linux 运行验收](e2e/e2e-linux-runtime.md)
- [静态检查](e2e/e2e-static-checks.md)

## 开发

- [本地开发](development/local-development.md)
- [静态验证](development/static-verification.md)
- [编码规范](development/coding-standards.md)
- [前端开发](development/frontend-development.md)
- [后端开发](development/backend-development.md)
- [临时文件隔离规则](development/temp-file-policy.md)

## 归档

归档文档位于 [archive](archive/)。归档内容仅作历史参考，可能包含过时架构或旧部署方式，不可作为当前部署依据。

## 文档地图

- [文档索引](DOCS_INDEX.md)
- [文档状态](DOCS_STATUS.md)
- [历史文档目录](archive/legacy-docs/)

## 验收方式

修改文档后必须执行：

```powershell
powershell -NoProfile -File scripts\verify-static.ps1 -SkipDockerBuild
```

## 相关文档

- [静态验证](development/static-verification.md)
- [端到端工程验收](e2e/e2e-engineering-acceptance.md)

## 剩余风险或外部环境要求

Docker、Linux cgroup v2、nsjail、多机 worker 的运行验收必须在具备对应能力的环境中执行。
