# OJOS Orchestrator 文档索引

当前源码版本是 Orchestrator `1.0.0`。唯一功能状态源是[项目状态总结](completeness-summary.md)；其他文档只定义契约、操作方式或额外证据要求，不独立宣布 GO。历史 alpha、0.2 兼容记录和 `docs/evidence/*.json` 不代表当前实现。

## 首先阅读

- [项目状态总结](completeness-summary.md)：当前已实现能力与按需执行的规模/签名证据。
- [交付判定](release-candidate.md)：本地功能结论、原生 CLI 安装和额外发布证据边界。
- [Orchestrator v1.0 运维手册](orchestrator/operations-v1.md)：生产预检、Node Agent、健康/指标、备份恢复、容量和 24 小时门禁。
- [生产就绪证据](production-readiness.md)：需要声明生产规模或签名分发时使用的证据账本。
- [可选的上线证据](unfinished/README.md)：需要对应声明时才从外部环境取得的两类 artifact。

## 架构与契约

- [架构总览](architecture/README.md)：v1 模块、执行路径和状态所有权。
- [耦合决策](architecture/coupling-decisions.md)：pure core、storage/control-plane/runtime/manager/agent 边界。
- [产品需求](orchestrator/requirements.md)：正式交付形态、对象、action 和非目标。
- [编排器边界](orchestrator/boundary.md)：控制面、Gateway、Store、Topology 和兼容层边界。
- [Action 模型](orchestrator/action-model.md)：published action、HTTP 契约和 0.2 兼容边界。
- [Operation/Job 模型](orchestrator/operation-model.md)：状态机、lease、恢复和补偿。
- [Topology 模型](orchestrator/topology-model.md)：Spec/Revision/Status、diff/apply/rollback 和 drift。
- [Service Contract v2](orchestrator/service-contract-v2.md)：Release v2、ApiBinding、ServiceContext、RuntimeReport、ApiResourceRef 和跨节点扩展规则。
- [工作负载凭据边界](orchestrator/credential-boundary-v2.md)：Node mTLS、Deployment JWT、Gateway 校验与管理凭据隔离。
- [编排器数据库](orchestrator/database.md)：Memory/SQLite/PostgreSQL、迁移、事务和旧数据导入。
- [Agent protocol v1](../platform/schemas/orchestrator/agent-protocol-v1.yaml)：Node enroll、claim、heartbeat、complete 和证书生命周期。
- [OpenAPI v1](../platform/schemas/orchestrator/openapi-v1.yaml) / [published actions](../platform/schemas/orchestrator/actions-v1.yaml)：正式机器契约。

## 客户端

- [Desktop 本地应用](orchestrator/desktop.md)：Tauri WebView、embedded backend、SQLite、制品资源布局，以及 managed execution 明确不可用的边界。
- [Desktop、Web 与 TUI 能力一致性](orchestrator/gui-tui-parity.md)：身份、published action 和共同协议规则。
- [Web UI 与 Store](orchestrator/web-ui.md)：Catalog v2、Store/Topology 页面、SSE 和持续运行门禁。
- [Web 开发说明](../manager/web/README.md)。
- [TUI 使用说明](../manager/tui/README.md)。
- [Service SDK](../sdk/service-sdk/README.md)：Go/Rust context client、token reload 与校验下载。

## 运维、发布与证据

- [v1 运维手册](orchestrator/operations-v1.md)：远程生产唯一正式运维入口。
- [生产运维脚本](../deploy/ops/README.md)：preflight、备份恢复、Docker Agent E2E 和 capacity/soak runner。
- [A/B 跨机完整门禁](../deploy/cross-machine/README.md)：单机双 Engine 的 production-equivalent 业务闭环及证据边界。
- [Judge Worker 生产部署](../deploy/worker/README.md)：B 节点 Agent、runtime policy、Catalog Release 和网络边界。
- [可核对证据索引](release/evidence.md)：实现、测试和 workflow 入口。
- [发布文档说明](release/README.md)：当前与历史发布记录的边界。
- [Staging 演练历史记录](evidence/staging-drill.md)：旧整栈演练范围，仅供历史核对，不是 v1 GA 证据。
- [机器可读历史快照](evidence/)：`production-readiness.json`、`release-candidate.json`，不得作为当前候选结论。

`ops/deployment-checklist.md`、`ops/ops-runbook.md`、旧 staging/rollback drill 面向历史 beta/整套 OJ 部署；Orchestrator v1 生产部署应使用 `orchestrator/operations-v1.md`。

## Service 规范与 OJ 边界

- [Service 规范](spec/service-spec.md)：Release v2 的当前字段与 Service v1 身份兼容边界。
- [Set 规范](spec/set-spec.md)：推荐部署组合；不属于 v1 运行时对象。
- [Endpoint / Link 规范](spec/endpoint-link-spec.md)：Topology v1 Endpoint/Link 与 ApiBinding 契约。
- [基础 Service 列表](services/README.md)：OJ 平台服务与边界。

## 历史文档

- [2026-07 重构记录](release/refactor-2026-07.md)：冻结当时实现与缺口；其中进程内 TUI、共享 token Node push、NoTls/无连接池、全局 console 锁和 Deferred provider 等表述均不代表 v1 当前状态。
- [v0.1.0 Alpha 快速上手](alpha-quickstart.md)：历史下载与使用说明，仍使用旧原生 GUI。

历史文档保留是为了迁移和考古。若历史表述与 v1 文档冲突，以当前 v1 源码、OpenAPI/action 契约和本页“首先阅读”中的状态文档为准。
