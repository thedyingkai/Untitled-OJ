# Service-first 后续计划

当前正式运行模型已经切换为 Service-first。旧 Module-first 设计已删除，不再作为正式代码、CLI、API、DB 初始化链路、包格式或验收入口。

后续工作只围绕 Service-first 模型继续：

- 完整实现 Native GUI。
- 完整实现 Non-root Agent 远程执行通道。
- 在干净 checkout 中重新执行 release gate，再决定是否创建发布 tag。
- 扩展 Endpoint、Link、Topology 的可视化和操作审计。
