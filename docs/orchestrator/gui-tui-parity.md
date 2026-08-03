# Desktop、Web 与 TUI 能力一致性

Orchestrator v1 只有一套正式控制契约：`/api/v1`、
`platform/schemas/orchestrator/openapi-v1.yaml` 和
`platform/schemas/orchestrator/actions-v1.yaml`。Desktop、浏览器中的 Web UI 与 TUI
都按控制面返回的 capabilities 工作，不能在客户端补出服务端没有发布的能力，也不能把
HTTP 接收成功显示成业务执行成功。

| 入口 | 运行方式 | 身份 | 持久化 |
| --- | --- | --- | --- |
| Desktop | Tauri WebView 内嵌同源 Web UI、backend 与 loopback Agent；不打开外部浏览器 | 一次性 bootstrap secret 换取 HttpOnly 本地 admin 会话 | OS 应用数据目录中的 SQLite、Agent ledger 与 artifact 目录 |
| 远程 Web | daemon 托管的同源 Vue UI | OIDC Authorization Code + PKCE，HttpOnly 会话 | PostgreSQL 控制面 |
| TUI | `/api/v1` 客户端，不在进程内执行 mutation | OIDC Device Authorization Grant；access token 只保存在内存 | PostgreSQL 控制面 |
| daemon API | REST + SSE | 固定 viewer/operator/admin RBAC | Desktop SQLite 或生产 PostgreSQL |

`--ephemeral` 和 TUI 的 `--legacy-local` 只用于开发、测试或 0.2 兼容，不是 v1
生产入口。Desktop SQLite 或生产 PostgreSQL 打开失败时均不得回退内存。

## 等价范围

Web 与 TUI 都覆盖已发布的完整 action 集合：

- Catalog：list、search、register、remove；
- Release/Store：import、validate、install、upgrade、rollback、delete；
- Node：register、revoke、list、health、drain、remove；
- Deployment：list、get、start、stop、restart、uninstall、health；
- Topology：draft、revision、Endpoint/Link 编辑、validate、diff、apply、rollback、status、export；
- Operation：plan、confirm、apply、cancel、retry、rollback、logs、events；
- Diagnostic：create、list、get、export。

Web 可以提供画布拖拽、筛选和可视化状态；TUI 使用表单、命令和分页列表表达相同控制能力，
不要求复制像素布局。画布坐标是按用户和 topology 保存的 UI state，不进入
`TopologySpec`。健康状态来自正式 `TopologyStatus`/Deployment health，不从 Endpoint
或 Link 的期望字段推断。

## 共同协议规则

- 所有 mutation 都携带 `Idempotency-Key`；异步 mutation 只在收到 `202` 且响应含
  `operation_id` 时视为已接受。
- `operation.plan` 返回 `201`；客户端随后按契约执行 confirm/apply，不能绕过计划。
- 集合读取保留服务端 cursor；客户端不能用本地数组下标模拟下一页。
- Topology revision 与 draft 编辑转发强 ETag/`If-Match`；`412` 必须显示为并发冲突。
- 错误按 `application/problem+json` 展示，保留 `request_id` 和 `operation_id`，不得改写为成功提示。
- Operation 事件通过 SSE 读取，重连保留 `Last-Event-ID`；Web 的轮询可取消，事件缓存有上限。
- capabilities 与 RBAC 是服务端真值。没有发布的 action 不出现在菜单、命令帮助或 optimistic fallback 中。

## 自动门禁

以下文件共同锁定能力矩阵：

- `platform/schemas/orchestrator/actions-v1.yaml`：发布 action 与 RBAC 真值；
- `manager/web/src/published-actions.ts`：Web SDK 方法映射；
- `manager/tui/src/remote.rs`：TUI 控制 action 清单；
- backend contract tests：router、OpenAPI、dispatcher 与 capabilities 对齐；
- Web/Vitest、Playwright 与 TUI fixtures：相同默认值、状态码、Problem、cursor、ETag、SSE 和幂等语义。

发布门禁要求三份 action 清单完全一致且 published `UNSUPPORTED` 为零。Playwright
还覆盖 Catalog 管理、Store 安装/升级/回滚/卸载、Node 生命周期、Topology 全流程、
Operation 日志/重试/取消、Diagnostic 与布局持久化；TUI contract tests 对相同 v1 路由和
响应 fixture 做等价验证。
