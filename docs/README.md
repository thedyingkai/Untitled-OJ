# OJOS 文档索引

本目录是 OJOS Orchestrator（OJOS 编排器）平台的正式中文文档。历史/废弃文档已删除，只保留当前
架构对应的内容。文档按主题组织如下。

## 架构（architecture/）

- [架构总览](architecture/README.md) —— service-release-first 架构与正式核心对象。
- [编排器需求](orchestrator/requirements.md) —— Orchestrator 管理的正式层与职责边界。
- [编排器边界](orchestrator/boundary.md) —— 编排器与 Gateway、业务服务、Root 角色的边界。
- [Action 模型](orchestrator/action-model.md) —— 分层 CRUD action 注册表、执行契约、能力状态。
- [Operation 模型](orchestrator/operation-model.md) —— 操作状态机与审计单元。
- [Topology 模型](orchestrator/topology-model.md) —— 拓扑视图与健康派生。
- [编排器数据库](orchestrator/database.md) —— 编排器数据库边界与正式表。
- [GUI / TUI 等价性](orchestrator/gui-tui-parity.md) —— 管理入口的能力等价约束。

## 规范（spec/）

- [Service 规范](spec/service-spec.md) —— `service.yaml` 身份契约。
- [Set 规范](spec/set-spec.md) —— 推荐部署组合。
- [Endpoint / Link 规范](spec/endpoint-link-spec.md) —— `ip:port:service-name` 连接身份与授权关系。

## 服务（services/）

- [基础 Service 列表](services/README.md) —— 平台基础服务与边界。

## 运维（ops/）

- [部署清单](ops/deployment-checklist.md) —— 首个生产候选 / beta 的部署步骤。
- [运维手册](ops/ops-runbook.md) —— 生产候选 / beta 运维排障。

## 发布与证据（release/、evidence/）

- [发布门禁](release/README.md) —— 发布前核对项。
- [可核对证据](release/evidence.md) —— 当前实现证据集与已知限制。
- [生产就绪证据](production-readiness.md) —— 门禁矩阵与密钥生命周期。
- [发布候选证据](release-candidate.md) —— 发布判定与模块完成度自评。
- [Staging 备份/恢复/回滚演练](evidence/staging-drill.md) —— 演练说明。
- [机器可读证据](evidence/) —— `production-readiness.json`、`release-candidate.json`（数据，不作正文）。

## 发布与上手

- [v0.1.0 Alpha 快速上手](alpha-quickstart.md) —— 下载运行编排器可执行文件、按 service 看效果、拉取 service 下载。

## 完成度与未完成事项

- [项目完成度总结](completeness-summary.md) —— 逐模块完成度、功能用法、对标生产的缺陷。
- [未完成事项](unfinished/README.md) —— 尚未完成、需要后续设计或实现的内容。
