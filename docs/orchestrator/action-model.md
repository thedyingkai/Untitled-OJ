# Orchestrator v1 Action 模型

正式 action 清单以 `platform/schemas/orchestrator/actions-v1.yaml` 为唯一来源，并由 OpenAPI、router、dispatcher、RBAC、Web 和 TUI 的契约测试共同校验。发布矩阵中不得出现 `UNSUPPORTED`；不可用能力不得进入 capabilities 或客户端界面。

## Published action

- catalog/release：Catalog 来源管理、Release 导入/校验/安装/升级/回滚/删除；
- node：注册、续签、列表、健康、drain、证书吊销和移除；
- deployment：查询、启动、停止、重启、卸载和健康；
- topology：draft、revision、validate、diff、apply、rollback、status 和 export；
- operation：plan、confirm、apply、cancel、retry、rollback、logs/events；
- diagnostic：create、list、get 和 export。

Endpoint/Link 的 mutation 只编辑 Topology draft。route、frontend、migration、permission、redis、storage、config 和 secret 是签名 Release pipeline 的类型化内部步骤，不再以独立 CRUD 的“登记成功”冒充外部执行成功。

## HTTP 契约

- 正式前缀只有 `/api/v1`；成功响应包含 `request_id`，失败为 `application/problem+json`。
- `plan` 和资源创建返回 `201`；长操作返回 `202` 与 `operation_id`。
- mutation 强制 `Idempotency-Key`；集合使用 cursor；Revision 使用 ETag/`If-Match`。
- Operation 状态与日志通过支持 `Last-Event-ID` 的 SSE 输出。
- Web 和 TUI 只能根据 capabilities 中的 published action 渲染入口，不根据旧 action catalog 猜测能力。

## 兼容边界

`0.2.0` 兼容构建可启用 `legacy-0_2`，无版本旧路由必须带弃用和替代路径响应头。`1.0.0` 中旧 mutation 返回 `410 Gone`，旧 Node push/bearer 路径不存在。

旧 `platform/schemas/orchestrator/actions.yaml`、`forms.yaml` 和通用 `/actions` 只属于 0.2 兼容层，不是 v1 发布契约。它们可以保留旧记录的读取/迁移能力，但不得进入 v1 capabilities、Web/TUI 菜单或发布矩阵。
