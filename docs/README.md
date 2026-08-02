# OJOS 文档索引

这里收录 OJOS Orchestrator 的维护文档。架构、契约和运维说明以当前源码为准；带版本号的发布文档只描述
对应历史版本。

## 架构（architecture/）

- [架构总览](architecture/README.md)：service-release-first 架构与主要对象。
- [耦合决策](architecture/coupling-decisions.md)：core、外部驱动与仍需收口的边界。
- [编排器需求](orchestrator/requirements.md)：正式对象、入口和职责范围。
- [编排器边界](orchestrator/boundary.md)：编排器与 Gateway、业务服务、Root 角色的边界。
- [Action 模型](orchestrator/action-model.md)：action 目录、执行契约和能力状态。
- [Operation 模型](orchestrator/operation-model.md)：操作状态机、确认和回滚。
- [Topology 模型](orchestrator/topology-model.md)：拓扑来源、节点、API 路由和健康状态。
- [编排器数据库](orchestrator/database.md)：数据库所有权和正式表。
- [入口形态与能力边界](orchestrator/gui-tui-parity.md)：Web UI、TUI、daemon 的分工。
- [Web UI 与插件商店](orchestrator/web-ui.md)：构建、页面、商店 API 和运行限制。

## 规范（spec/）

- [Service 规范](spec/service-spec.md)：`service.yaml` 身份契约。
- [Set 规范](spec/set-spec.md)：推荐部署组合。
- [Endpoint / Link 规范](spec/endpoint-link-spec.md)：`ip:port:service-name` 身份与授权关系。

## 服务（services/）

- [基础 Service 列表](services/README.md)：平台基础服务与边界。

## 运维（ops/）

- [部署清单](ops/deployment-checklist.md)：beta 部署步骤和放量前检查。
- [运维手册](ops/ops-runbook.md)：健康检查、排障、备份和回滚。

## 发布与证据（release/、evidence/）

- [发布门禁](release/README.md)：发布前核对项。
- [可核对证据](release/evidence.md)：本地验证、远端运行和已知限制。
- [2026-07 重构记录](release/refactor-2026-07.md)：Web UI、商店、生命周期和安全加固。
- [生产就绪证据](production-readiness.md)：门禁矩阵与密钥生命周期。
- [发布候选证据](release-candidate.md)：发布判定与模块自评。
- [Staging 备份/恢复/回滚演练](evidence/staging-drill.md)：演练范围和产物。
- [机器可读证据](evidence/)：`production-readiness.json`、`release-candidate.json`。这些文件是历史快照，
  不能替代当前提交的测试结果。

## 发布与上手

- [v0.1.0 Alpha 快速上手](alpha-quickstart.md)：历史版本下载与使用说明。该版本仍使用原生 GUI，不含当前 Web UI。

## 完成度与未完成事项

- [项目状态总结](completeness-summary.md)：当前能力、验证范围和生产缺口。
- [未完成事项](unfinished/README.md)：需要继续设计或实现的内容。
