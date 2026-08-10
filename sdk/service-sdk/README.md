# Service Contract v2 SDK

新 Service 不拼接 Gateway URL，不读取全局管理 token，也不直连远端数据库或中间件。它声明 Release v2 requirement，并使用共享 SDK 从 Agent 物化的 `/run/ojos/service/context.json` 中按 requirement 名调用 provider。

## 实现位置

- Go：`platform/shared/go/servicecontext`；
- Rust：`platform/shared/rust/service-context`（crate `ojos-service-context`）；
- 机器契约：`platform/schemas/orchestrator/service-context-v1.schema.json` 与 `api-resource-ref-v1.schema.json`。

两种 SDK 都提供：

- 加载、校验 ServiceContext，并核对 deployment/service/node；
- 按 Binding 名解析 `/internal/apis/<api-id>` 基础路径；
- 使用 context 中的 CA 与 per-binding timeout 构造 HTTP client；
- 每次请求重新读取 `/run/ojos/service/token`，因此 Agent 轮换 JWT 不要求重启容器；
- 附加 trace context和幂等请求头；
- 流式下载到临时文件，校验预期 SHA-256/size 后原子发布目标文件；
- 拒绝绝对 URL、越界相对路径、未知 Binding 和半写 context/token。

Go 的主要入口是 `LoadOptional`/`Load`、`BindingURL`、`NewRequestWithOptions`、`DoWithOptions` 和 `DownloadTo`。Rust 的对应入口是 `ServiceContext::load_optional`/`load`、`binding_url`、`request`、`authorize` 和 `download_to`。

## 新增普通 Service

1. 在相邻 `release.yaml` 使用 `schema_version: 2`，声明 `provides.apis`、`requires.apis`、events 和 `standard-container-v1`。
2. 业务代码只引用稳定 requirement 名，例如 `storage_get`；不要写 provider host、Gateway origin 或 `/internal/apis/...`。
3. 提供 manifest 对应的 OpenAPI/事件 Schema 和真实健康检查。
4. 构建 digest-pinned OCI image，并生成带 metadata SHA-256 和 Ed25519 签名的 Catalog v2。
5. 在 Store 中选择目标 Node 和 provider 候选，确认 prospective Topology diff，再由正常 apply 激活 Binding。
6. 用最小 provider/consumer fixture 验证跨 Node 调用、断链失效、provider rebind、token 轮换和资源校验。

只有确实需要特殊 HostConfig 的工作负载才新增版本化 runtime profile。Release 与安装请求不得自定义 privileged、capability、host path 或 security option。

字段规范见 [Service 与 Release v2 规范](../../docs/spec/service-spec.md)，完整架构见 [Service Contract v2](../../docs/orchestrator/service-contract-v2.md)。
