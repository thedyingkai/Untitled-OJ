# Service Contract v2：跨节点服务接入

Service Contract v2 是 Store、Topology、Gateway 与 Agent 之间唯一的生产接入层。业务服务只声明“提供什么、依赖什么”，不保存 Gateway 管理凭据，也不拼装其他服务的主机地址。

正式线协议由仓库内的版本化 Schema 固化：`service-contract-v2.schema.json`、`api-binding-v1.schema.json`、`service-context-v1.schema.json`、`runtime-report-v1.schema.json`、`api-resource-ref-v1.schema.json`，以及 `platform/schemas/events` 下的 CloudEvents Schema。实现不得在这些边界传递未声明字段。

## 运行链路

1. Release 的 `provides.apis` 声明 provider-native 路径、SemVer、认证方式和权限。
2. `requires.apis` 用稳定 requirement 名声明依赖。业务代码只引用 requirement 名。
3. Store validate 从已应用拓扑、签名 Release、真实 RuntimeInstance 和新鲜 Agent facts 中生成候选。零候选拒绝；多个健康候选必须显式选择。
4. 用户确认后，Store Operation 创建不可变 Topology revision。Binding、Gateway 路由和 workload identity 都随同一 apply saga 暂存、激活或补偿。
5. Agent 为 Deployment 物化只读 `/run/ojos/service`。上下文包含 Gateway origin、CA、命名 Binding 和 generation，但不包含管理 token。
6. Agent 用 Node mTLS 和 Deployment assignment 换取 15 分钟 JWT，并在剩余 5 分钟前原子替换 token 文件。容器无需重启。
7. SDK 每次请求重新读取 token，经 `/internal/apis/<api-id>` 调用 Gateway。Gateway 使用 JWT 中的 Deployment 与 generation 精确匹配活动 Binding，再转发到 provider-native 路径。

断开 Link、卸载 consumer 或切换 provider 时，控制面提升该 Deployment 的 generation；旧 JWT 即使未过期也不能再命中活动路由。

## 新增普通 Service

普通容器使用固定 `standard-container-v1`，接入时只需：

- 提交合法的 `release.yaml`（`schema_version: 2`）；
- 在 `provides.apis` 和 `requires.apis` 中声明契约；
- 使用 Go 或 Rust service-context SDK；
- 为每个公开 API 提供与 manifest 一致的实现和健康检查；
- 构建 digest-pinned OCI image，并用 Catalog v2 生成器产出签名 metadata/catalog；
- 在 Topology draft 中连接 consumer Endpoint 与 provider Endpoint。

不应新增 Orchestrator action、Gateway 专用分支、共享 token 环境变量或远程数据库连接。只有确实需要特殊 HostConfig 的工作负载才新增不可变 runtime profile；安装请求不能自定义 capability、host path 或 security option。

## Catalog 生成

`generate_service_contract_catalog` 从 v2 manifest、不可变 OCI digest 和 Ed25519 seed 生成 metadata、Catalog、trust 与 source 文件。输出目录必须不存在，生成过程不会覆盖旧的签名候选。

```text
cargo run -p orchestrator-manager \
  --example generate_service_contract_catalog -- \
  --output <new-directory> \
  --release-manifest services/judge-worker/release.yaml \
  --signing-key-file <protected-ed25519-seed-file> \
  --public-base-url https://catalog.example/ojos/judge-worker \
  --oci-image registry.example/ojos/judge-worker@sha256:<64-hex>
```

Store production import 会验证 Catalog 的 RFC 8785/JCS Ed25519 签名、metadata SHA-256、平台/最低版本和 OCI RepoDigest。`local://`、空 image 与浮动 tag 只存在于源码模板，不能成为生产安装输入。

## ApiResourceRef

持久任务中的大文件引用遵循 `platform/schemas/orchestrator/api-resource-ref-v1.schema.json`：

```json
{
  "binding": "storage_get",
  "api_id": "storage.object.get",
  "relative_path": "/problems/package-sha256-<digest>.zip",
  "sha256": "sha256:<64-hex>",
  "size_bytes": 123456,
  "content_type": "application/zip"
}
```

任务不得携带绝对 URL、`/internal/apis/...` URL、共享目录或管理凭据。SDK 按当前 Binding 解析资源，流式写入临时文件，校验 size/SHA-256 后原子发布目标文件。

## Judge Worker 特例

Judge Worker 使用固定 `judge-sandbox-v1`。只有签名、digest-pinned 的 Judge Release 可以引用它，且 B 节点本地 Agent policy 必须同时允许 profile ID 与 digest。该 profile 固定包含 nsjail 当前所需的 privileged、host cgroup namespace、三项 capability、AppArmor 例外和 cgroup mount；它不会从安装请求接受任意 Docker 参数。

生产 B 节点只需要向控制面 Agent API、A Gateway、OCI registry、DNS/时间服务出站连接。Worker 不接收 PostgreSQL、Redis、MinIO、Auth/Gateway 管理凭据。`deploy/worker/docker-compose.yml` 仅是本地开发兼容入口。

## 接入门禁

每个新 Service 至少验证：

- manifest v2 lint、SemVer 兼容、零/单/多 provider；
- stale Topology ETag、确定性 diff、apply 失败补偿与断链失效；
- context/token 原子轮换和 Agent ledger 恢复；
- SDK 路径约束、超时、trace、idempotency 与资源 checksum；
- provider 升级健康后原子切换，失败时保持旧 provider；
- Deployment/Binding/health/drift 在 Web 与 TUI 中状态一致；
- 跨 Engine 网络策略证明 consumer 只能经 Gateway 访问业务 provider。
