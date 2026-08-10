# Service 与 Release v2 规范

OJOS 把“服务身份”和“可部署版本”分开描述：`service.yaml` 保留稳定的 Service 身份与业务边界，`release.yaml` 使用 Service Contract v2 描述某个版本如何运行、提供什么 API、依赖什么 API/事件，以及使用哪个不可变 runtime contract。

机器可读真值是：

- `platform/schemas/orchestrator/service-contract-v2.schema.json`；
- `platform/schemas/orchestrator/api-binding-v1.schema.json`；
- `platform/schemas/orchestrator/service-context-v1.schema.json`；
- `platform/schemas/orchestrator/runtime-report-v1.schema.json`；
- `platform/schemas/orchestrator/api-resource-ref-v1.schema.json`；
- `platform/schemas/events/*.schema.json`。

## `service.yaml`

`service.yaml` 当前仍是 schema v1 的身份兼容层，记录 Service ID、版本、类型、默认 Endpoint、业务能力、健康要求和安全上限。它不能请求任意 command、脚本、hook、`privileged`、`cap_add` 或 host mount；特殊 Docker 语义只能通过编排器内置、版本化且签名引用的 runtime profile 获得。

同目录校验要求 Service ID、SemVer、类型、backend protocol/port 和健康路径与 `release.yaml` 一致。`service.yaml` 不授予跨服务调用权限，也不能写全局 Topology。

## `release.yaml` v2

生产 Release 使用 `schema_version: 2`。下例省略 migration、route、storage 等普通交付字段，展示正式服务契约：

```yaml
schema_version: 2
service_name: example-consumer
version: 1.2.3
service_type: backend-worker

source:
  kind: url
  url: https://catalog.example/releases/example-consumer-1.2.3.tar
  checksum: sha256:<64-hex>

runtime:
  kind: image
  image: registry.example/ojos/example-consumer@sha256:<64-hex>

backend:
  protocol: http
  port: 9101
  health_path: /healthz/ready

provides:
  apis: []

requires:
  apis:
    - name: storage_get
      id: storage.object.get
      version: ">=1.0.0, <2.0.0"
      optional: false
      selection: explicit
      timeout_ms: 300000

events:
  publishes: []
  subscribes: []

runtime_contract:
  id: standard-container-v1
  sha256: sha256:<64-hex>
  binding_directory: /run/ojos/service
  identity_mode: workload
  credential_delivery: file
  restart_on_change: false
```

### Provided API

每个 `provides.apis` 项至少包含稳定 `id`、SemVer `version`、以 `/` 开头的 provider-native `path`、`auth` 和 `permission`。可选字段包括 protocol、port name、methods、visibility、stability 和不超过 300000 ms 的 timeout。生产 workload API 使用 `auth: workload`；人类用户 API 使用 `auth: user`。

### Required API

每个 `requires.apis` 项至少包含：

- `name`：业务代码使用的稳定 requirement 名；
- `id`：provider API ID；
- `version`：非空 SemVer 范围；
- `timeout_ms`：1–300000；
- `optional` 与 `selection`：`nearest-healthy`、`same-node` 或 `explicit`。

Store validate 只从 applied Topology、签名 Release、Running/Healthy RuntimeInstance 和新鲜 RuntimeReport 中给出候选。零候选拒绝；多个候选必须显式选择；唯一推荐也必须随 Operation 确认。调用授权来自最终 applied ApiBinding，不来自 manifest 中的 caller allowlist 或共享 service token。

### Events

事件声明使用稳定 type、SemVer、schema ref 和 consumer group。Problem/Judge 的正式事件是完整 CloudEvents snapshot/tombstone；producer transactional outbox 与 consumer inbox/projection 提供至少一次传递下的幂等，不能用跨数据库手工 SQL 替代。

### Runtime contract

普通服务使用 `standard-container-v1`。Judge Worker 使用 `judge-sandbox-v1`；只有签名、digest-pinned 的 Release 以及节点本地精确 allowlist 才能启用。Release 或安装请求不能覆盖 capability、mount、security option、host path 或 profile 内容。

## 生产 artifact 规则

仓库内 `release.yaml` 可以保留 `local://`、空 image/checksum 作为源码模板，但不能直接作为生产安装输入。生产 Catalog 必须把 v2 manifest、metadata SHA-256、OCI RepoDigest、平台/最低编排器版本和 Ed25519 签名绑定在一起；Store 验证失败时在任何 Docker 或 provider 副作用前拒绝。

生成签名 Catalog 的命令和约束见 [Service Contract v2](../orchestrator/service-contract-v2.md)。新增 Service 的 SDK 接入见 [Service SDK](../../sdk/service-sdk/README.md)。

## v1 兼容导入

旧 Release 的顶层 `apis`、`required_apis` 和 `service_identity.allowed_apis` 只用于兼容读取。导入器可以将无歧义字段转换成 `provides.apis`/`requires.apis`，但生产安装前必须得到稳定 requirement 名、SemVer 范围、timeout、provider 选择和 runtime contract；有歧义时必须拒绝，不能登记成“已安装”。
