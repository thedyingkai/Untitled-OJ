# OJOS Orchestrator

OJOS Orchestrator（OJOS 编排器）是 OJOS 的服务控制面。它负责导入服务契约、生成变更计划、维护连接与运行状态，并提供观测和诊断入口。

它不实现题库、提交、比赛、用户、公告、训练、Clarification、打印或滚榜。这些业务属于具体 Service；编排器只管理 Service 及其发布、端点、连接、操作记录和拓扑。

## 核心对象

理解运行时行为时，最常用的对象有：

- ServiceRelease：可校验、导入和安装的服务发布契约。
- Host：运行 Service 的主机或节点。
- Service：最小可安装、可启停、可连接、可观测的功能单元。
- Set：推荐部署组合，只描述组成和默认关系，不作为运行时对象。
- Endpoint：运行中 Service 的唯一连接身份，格式固定为 `ip:port:service-name`。
- Link：`source endpoint -> target endpoint` 的通信授权关系。
- Operation：需要计划、确认、执行、记录和回滚的编排动作。
- Topology：Service、Set、Endpoint、Link、Operation、LogView、DiagnosticReport 的关系视图。
- LogView：按 Service、Endpoint 或 Endpoint host/IP 聚合的日志视图。
- DiagnosticReport：部署和运行诊断结果。

## 正式入口

正式入口包括 Orchestrator Web UI（daemon 内嵌托管）、Orchestrator TUI 和 Orchestrator daemon HTTP API。三者使用同一套 `services/orchestrator/core` 和 `platform/schemas/orchestrator`。

- **Web UI**（`manager/web`）：主入口，提供拓扑编辑、商店、服务生命周期和操作审计。daemon 直接托管构建产物。
- **TUI**（`manager/tui`）：无浏览器的服务器场景使用。
- **daemon**（`services/orchestrator/backend`）：提供 HTTP API 和静态文件服务，使用固定大小的工作线程池处理连接。

原 egui GUI 已删除，由 Web UI 取代。

### 快速开始

```bash
# Node.js 需要 ^22.18.0 或 >=24.11.0。
cd manager/web
npm ci
npm run typecheck
npm run build
cd ../..

# 启动 daemon；开发环境未设置数据库时使用内存 store。
cargo run -p ojos-orchestrator-daemon -- --repo-root . --bind 127.0.0.1:8090

# 打开浏览器。
# http://127.0.0.1:8090/
```

`GET /health` 不需要令牌。设置 `ORCHESTRATOR_INTERNAL_TOKEN` 后，其余 API 都要携带
`x-ojos-orchestrator-token`；未设置时控制面不做认证，只适合本机开发。需要保留拓扑和 Operation 时，还要设置
`ORCHESTRATOR_DATABASE_URL`。

Gateway 是业务流量入口 Service，不是控制面。Gateway frontend 是 OJ 业务 UI，不是 Orchestrator 入口。

## 插件商店

编排器本体不内置业务模块。商店从索引或 GitHub Release 导入 zip、tar.gz 或 `release.yaml`：

- 商店索引：`OJOS_STORE_INDEX_URL` 指向索引 JSON（GitHub raw 地址或仓库内相对路径，默认 `store/index.json`）。
- GitHub 安装：在 Web UI 中输入 `owner/repo`，选择 Release 资产；`OJOS_GITHUB_TOKEN` 可用于提升 API 配额或访问私有仓库。
- 打包模块：`deploy/release/pack-service-package.sh <service>` 生成可上传到 GitHub Release 的模块包。
- 相关 API：`GET /store/index`、`GET /store/github/releases?repo=…`、`POST /store/import`、`POST /store/install`。

当前服务包主要携带契约和迁移文件，不等于可运行二进制或镜像。`LocalProcessDriver` 和
`DockerComposeDriver` 还需要源码、命令或 Compose 文件等运行资产；最小 daemon 镜像和历史 alpha bundle
不包含这些资产。若服务已经由外部系统启动，可在安装请求中传
`external_service_running=true` 登记它；端点必须真实可达，而且不能覆盖仍可能在运行的旧部署。该选项与
`execute_service_driver=true` 互斥，登记后运行时所有者是 `external`，本控制面不会用本地 driver 启停或删除它。
另一种做法是把经过审核的运行目录显式挂载进运行环境，再使用固定 driver。

## 数据库边界

Orchestrator 使用独立数据库：

```text
ORCHESTRATOR_DATABASE_URL
```

OJ 业务服务使用各自的服务级数据库：

```text
AUTH_DATABASE_URL
PROBLEM_DATABASE_URL
JUDGE_DATABASE_URL
USER_DATABASE_URL
```

Orchestrator 不写 service-owned 业务表；OJ 业务服务也不能直接写 Orchestrator 表。

## 基础 Service

当前基础 Service 包括：

```text
gateway
auth-service
problem-service
user-service
judge-api
judge-worker
postgresql
redis
storage-service
minio
jaeger
orchestrator
```

每个 Service 必须提供 `service.yaml` 和相邻 `release.yaml`。`service.yaml` 是正式 Service 身份契约；`release.yaml` 是发布/导入契约，并且 route、version、backend protocol/port 必须与 `service.yaml` 对齐。

## 文档

正式文档位于 `docs/`，完整索引见 [docs/README.md](docs/README.md)：

- [文档索引](docs/README.md)
- [项目完成度总结](docs/completeness-summary.md)
- [未完成事项](docs/unfinished/README.md)
- [Service 规范](docs/spec/service-spec.md)
- [Set 规范](docs/spec/set-spec.md)
- [Endpoint / Link 规范](docs/spec/endpoint-link-spec.md)
- [Orchestrator 需求](docs/orchestrator/requirements.md)
- [Orchestrator 边界](docs/orchestrator/boundary.md)
- [Action 模型](docs/orchestrator/action-model.md)
- [入口形态与能力边界](docs/orchestrator/gui-tui-parity.md)
- [Web UI 与插件商店](docs/orchestrator/web-ui.md)
- [Topology 模型](docs/orchestrator/topology-model.md)
- [Operation 模型](docs/orchestrator/operation-model.md)
- [Orchestrator 数据库](docs/orchestrator/database.md)
- [部署清单](docs/ops/deployment-checklist.md)
- [运维手册](docs/ops/ops-runbook.md)
- [可核对证据](docs/release/evidence.md)
- [2026-07 重构记录](docs/release/refactor-2026-07.md)

`v0.1.0-alpha` 是 2026-07-03 发布的历史版本，仍使用原生 GUI，不包含当前 Web UI。下载说明见
[历史 Alpha 快速上手](docs/alpha-quickstart.md)。
