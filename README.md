# OJOS

OJOS 是一个 Installer-first 的分布式 OJ Service Runtime，由唯一 Root Installer / Runtime Manager 维护全局状态，并以 Service、Set、Endpoint、Link、Device、Topology 为核心对象。

## 项目定位

OJOS 不以 Web Shell、Gateway 或旧 Module-first 模型作为系统根。Root Installer / Runtime Manager 是控制面；Service 是最小安装、运行、启停、热插拔和连接单位。

核心对象：

- Service：最小功能单元，例如 gateway、web-shell、problem-api、judge-api、judge-worker、storage、postgres。
- Set：推荐安装集合，不是运行对象。
- Endpoint：运行中 Service 的 `IP:Port`。
- Link：`source endpoint -> target endpoint` 的连接关系。
- Device：Root 或 Non-root 设备。
- Topology：Device、Service、Endpoint、Link、Set、Health 和 Operation 的关系视图。

## 基础服务

- Gateway：外部 HTTP 入口、鉴权、权限校验、路由转发、统一错误、审计和基础限流。
- Web Shell：Root 侧可热插拔 Web UI，只展示题库、提交、评测结果、普通管理和只读 Runtime 状态。
- Problem API：题库、题目详情、题目包、数据文件索引和题目权限。
- Judge API：提交、任务队列、Worker endpoint 列表、任务分发、结果接收和状态更新。
- Judge Worker：Root 或 Non-root 上的独立评测服务，内部管理并发和 sandbox slots。
- Storage / PostgreSQL：即使使用外部实例，也作为可连接 Service 出现在 Runtime、Endpoint、Link 和 Topology 中。

## 部署入口

单机部署使用 `sets/single-node-oj.yaml`：

```powershell
docker compose --env-file .env -f deploy\compose\docker-compose.yml up -d --build
```

分布式评测中，Root 设备使用 `sets/distributed-root.yaml`；评测机使用 `sets/judge-worker-node.yaml`，只运行 Non-root Device Agent 和 judge-worker，不运行 Web Shell 或 Root Installer GUI。

## 命令入口

```powershell
cargo run -p ojosctl -- service discover
cargo run -p ojosctl -- service validate services\judge-worker\service.yaml
cargo run -p ojosctl -- set expand sets\single-node-oj.yaml
cargo run -p ojosctl -- endpoint validate 192.168.1.10:8082
cargo run -p ojosctl -- link plan-create 192.168.1.21:9101 192.168.1.10:8082
```

旧 Module-first 设计已删除，不再作为正式运行模型、CLI、API、DB 初始化链路或包格式。

## 当前完成能力

- `service.yaml` 契约和基础 Service 描述已建立。
- Set 预设已建立。
- Endpoint / Link / Topology 命令级计划能力已建立。
- Root Runtime Manager 数据表与 service-first API 已建立。
- Web Shell 已调整为只读 Runtime / Topology / Service 状态视图，不作为 Installer。

## 未完成边界

- Native GUI 目录和边界已建立，完整 GUI 交互仍需后续实现。
- Non-root Device Agent 远程执行通道仍需后续实现。

## 文档索引

入口文档见 [docs/DOCS_INDEX.md](docs/DOCS_INDEX.md)，状态说明见 [docs/DOCS_STATUS.md](docs/DOCS_STATUS.md)。
