# Service 规范

Service 是 OJOS Orchestrator 管理的最小功能单元。题目、提交、比赛、用户、权限和公告等业务能力归各 Service；Orchestrator 负责校验契约、安排运行位置，并维护 Endpoint、Link、Operation、Topology、日志和诊断报告。

每个 Service 目录同时放置 `service.yaml` 与 `release.yaml`。前者描述“它是什么”，后者描述“这个版本怎么交付和注册”。

## `service.yaml`

```yaml
schema_version: 1
id:
name:
version:
kind:
description:

runtime:
  mode:
  driver:
  root_allowed:
  non_root_allowed:
  start_policy:
  restart_policy:

endpoint:
  protocol:
  default_port:
  health_path:
  expose:
  routes:

requires:
  services:
  links:
  optional_links:
  storage:
  database:
  queue:
  secrets:

provides:
  capabilities:
  endpoints: []
  routes:
  workers:
  storage_buckets:
  events:

config_schema:
resources:

security:
  allow_privileged: false
  allow_host_mount: false
  allow_arbitrary_command: false
  required_secrets:
  sandbox:
  network_policy:

source:
  type:
  ref:
  build:
  artifact:

ui:
  enabled:
  menu_scope:
  routes:
  menus:
  permissions:

permissions:

health:
  checks:
  timeout_seconds:
  interval_seconds:
```

约束：

- `schema_version` 当前只能是 `1`。
- `id` 使用小写字母、数字和连字符；`version` 必须是 SemVer。
- `kind` 可取 `frontend`、`backend-api`、`backend-worker`、`gateway`、`database`、`cache`、`storage`、`external`、`agent`。
- `runtime.mode` 可取 `local-process`、`container`、`external`；`root_allowed` 与 `non_root_allowed` 至少有一个为 `true`。
- Endpoint 协议可取 `http`、`https`、`tcp`、`postgres`、`redis`，`default_port` 必须大于 0。
- `provides.endpoints` 必须为空。运行时 Endpoint 由 Orchestrator 按 `ip:port:service-name` 创建。
- `source.type` 可取 `local`、`git`、`github`、`release`、`external`，`source.ref` 必填。
- `health.checks` 不能为空，timeout 和 interval 必须大于 0。

`service.yaml` 禁止任意 command、脚本、hook、`privileged`、`cap_add` 和 host mount。`allow_privileged`、`allow_host_mount`、`allow_arbitrary_command` 必须保持 `false`。secret 字段只能列名称，不能写值。

## `release.yaml`

```yaml
schema_version: 1
service_name:
version:
description:
service_type:

source:
  kind:
  url:
  checksum:

runtime:
  kind:
  image:
  binary:
  system_service:
  command:
  args:
  working_dir:
  env:

frontend:
  enabled:
  route_prefix:
  remote_entry:
  menu_items:

backend:
  protocol:
  port:
  health_path:

migrations:
  - version:
    path:
    checksum:
    destructive:

permissions:

routes:
  - path:
    method:
    target_type:
    target:
    permission:

apis:
  - api_id:
    protocol:
    port_name:
    path_prefix:
    methods:
    visibility:
    auth_mode:
    permission:
    stability:
    version:
    grpc_service:
    stream_name:
    rate_limit:
    timeout:
    allowed_callers:
    denied_callers:

redis:
storage:
dependencies:
required_apis:

service_identity:
  service_name:
  allowed_apis:

config_schema:
secrets:

observability:
  metrics:
  jaeger:
```

`source.kind` 可取 `github-release`、`repo`、`url`、`local`。`runtime.kind` 可取 `image`、`binary`、`system-service`、`external`、`local-process`；local-process 必须提供受校验的 `command`，相对 `working_dir` 不能越出 package root。

release API 目前只接受 `auth_mode: public`、`user` 或 `service`。`service` 模式必须声明非 `public`
permission，Gateway 才能交给 auth-service 校验调用方凭据。`internal` 仍是 Gateway 的保留模式；内部请求签名
尚未启用，所以 manifest 校验会直接拒绝它，不会留下“安装成功但路由永远返回 403”的条目。
`auth.user.permission.check` 是 auth-service 的保留 API ID，其它 Service 的 release 不能声明它。

`rate_limit`、`timeout`、`allowed_callers` 和 `denied_callers` 目前会写入 API surface 契约，但还没有传播到
EffectiveApiRoute，也不会由 Gateway 执行。插件不能把这些字段当成已经生效的限流、超时或调用方 ACL；当前实际
边界仍由 visibility、Link、`auth_mode` 和 permission 决定。

生产环境启用 `ORCHESTRATOR_REQUIRE_RELEASE_CHECKSUM=1` 后，安装请求或 manifest 必须提供 SHA-256。checksum 缺失或不匹配都会在加载阶段失败。

## 两份契约如何对齐

同目录校验会检查：

- `service_name`、`version`、`service_type` 分别匹配 `service.yaml` 的 `id`、`version`、`kind`。
- `backend.protocol`、`port`、`health_path` 匹配 Endpoint 声明。
- release permissions 覆盖 Service 和 UI permissions。
- frontend 的启用状态与 UI 一致，启用时 `route_prefix` 覆盖 UI route。
- storage、dependency、Redis queue 和 secret 注册覆盖 `service.yaml` 的对应声明。
- release route 覆盖 Endpoint 和 provided route。
- `service_identity.allowed_apis` 只能引用 `required_apis` 中已经声明的 API。

`source`、构建产物和发布包是契约内容，不会因此变成新的 core 对象。内置 release 也不等于可运行部署：image/binary 为空，或 local-process 依赖仓库源码时，目标环境必须另行提供运行资产。

## 基础 Service

```text
gateway
auth-service
problem-service
user-service
judge-api
judge-worker
postgresql
redis
storage-service
minio
jaeger
orchestrator
```

校验实现位于 `services/orchestrator/core/src/service.rs`，可参考 `sdk/templates/service.yaml` 和任一 `services/*/release.yaml`。
