# 文档状态

当前正式文档以 Service-first 架构为准。旧 Module-first 设计已删除，不再作为正式运行模型、CLI、API、DB 初始化链路或包格式。

## 已完成

- README 已改为 Service-first 入口。
- 架构、Service、Runtime、Set、Installer、Release 和 Roadmap 主文档已建立。
- `service.yaml` 契约、Set 预设、Endpoint、Link、Device、Topology 和 Web Shell 边界已写入正式文档。
- 旧 Module-first 说明仅允许出现在 `docs/archive/` 历史资料或迁移说明中。

## 未完成边界

- Native GUI 仍需完整实现。
- Non-root Agent 远程执行通道仍需完整实现。
- Release gate 需要在干净 checkout 中重新执行后再决定是否创建发布 tag。
