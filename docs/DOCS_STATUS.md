# 文档状态

当前正式文档以 Service-first 架构为准。旧架构说明和历史发布材料已进入 `docs/archive/`，不再作为正式入口。

## 已完成

- README 已改为 Service-first 入口。
- 架构、Service、Runtime、Set、Installer、Release 和 Roadmap 主文档已建立。
- `service.yaml` 契约、Set 预设、Endpoint、Link、Device、Topology 和 Web Shell 边界已写入正式文档。

## 兼容说明

旧 `module.yaml`、旧 `module_*` 表和旧 `/admin/modules` API 仍作为 legacy compatibility 存在，用于迁移读取和旧验收过渡。

## 未完成边界

- 后端 API 仍需全面迁移到 `/admin/services`、`/admin/endpoints`、`/admin/links` 和 `/admin/topology`。
- Native GUI 仍需完整实现。
- Non-root Agent 远程执行通道仍需完整实现。
