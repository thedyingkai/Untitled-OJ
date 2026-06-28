# Module Runtime

> 文档状态：当前实现，v0.1.0 发布基线
> 适用范围：Kernel、Gateway、Web Shell、模块作者
> 最后更新：2026-06-28

Module Runtime 是 OJOS Kernel 能力。它读取 Module Registry 和 stored manifest，计算当前 enabled module 的运行态贡献，并导出 Runtime Snapshot。

## Runtime Snapshot v1

Runtime Snapshot `version: 1` 是当前模块贡献事实源：

```json
{
  "version": "1",
  "generated_at": "...",
  "modules": [],
  "permissions": [],
  "roles": [],
  "menus": [],
  "frontend_routes": [],
  "gateway_routes": [],
  "services": [],
  "workers": [],
  "storage_buckets": [],
  "health_checks": [],
  "components": [],
  "operations": [],
  "topology": {
    "nodes": [],
    "edges": [],
    "module_nodes": [],
    "dependency_edges": []
  },
  "warnings": []
}
```

默认只返回 enabled module 的 active contribution。`include_disabled=true` 只供管理员检查 disabled registry contribution，不用于 public runtime surface。

## Aggregation 规则

- 每个 contribution 都带 `module_id`，便于追踪来源。
- disabled module 不进入 active permission、menu、route、topology 或 service surface。
- roles、storage buckets、events、admin panels、scheduled jobs 和 manifest topology 可从 stored manifest 派生。
- 响应不得包含 secret、token、DB 连接串、本机绝对路径、Docker socket 路径或 package 内部路径。

## Gateway Route Hotplug

Gateway 从 Runtime Snapshot 构建动态路由表：

- core static routes 优先。
- enabled module route 才可 proxy。
- `service_id` 必须存在于 trusted service map。
- reserved prefixes 不允许被模块声明。
- duplicate/overlap/unknown service/auth mode 会进入 conflicts、warnings 或 blocked_by。
- `upstream_base` 默认不返回给普通管理视图。
- 原始 `Authorization` 不透传到模块服务，Gateway 只转发受控 actor/internal headers。

## Topology

Topology 从 Runtime Snapshot 派生，包含：

- module nodes
- dependency edges
- service/worker/component nodes
- gateway route nodes
- frontend menu/route nodes
- health nodes
- manifest-declared topology nodes/edges

Web Shell 只渲染 snapshot，不为未来模块硬编码拓扑。

## Service Runtime Foundation

当前 L2 foundation 已支持：

- `provides.services` 和 `provides.workers` 进入 Runtime Snapshot。
- Gateway Kernel Runtime 具备 list、state、plan-start、plan-stop、plan-restart、plan-reload、plan-health 接口。
- compose driver 只读取 trusted service config 和 allowlist。
- runtime plan 使用 argv array，不生成 shell string。
- metadata-only service 不能 start/stop/restart。
- route table 可结合 service state/health 标记 degraded 或 unavailable。

状态模型：

```text
DECLARED INSTALLED ENABLED STARTING RUNNING DEGRADED STOPPING STOPPED FAILED DISABLED UNKNOWN
```

## Controlled Apply

Gateway/Web 只生成计划和查看状态，不 apply。真实 apply 由 `ojosctl` 或未来 operator 读取 plan file 后执行：

```powershell
cargo run -p ojosctl -- runtime plan-restart problem-api --out .tmp/agent/scratch/problem-api-restart.json
cargo run -p ojosctl -- runtime apply-plan .tmp/agent/scratch/problem-api-restart.json --dry-run
cargo run -p ojosctl -- runtime apply-plan .tmp/agent/scratch/problem-api-restart.json --confirm
```

计划字段包括：

```text
plan_id
operation_id
module_id
service_id
action
driver
can_apply
apply_enabled
requires_confirmation
dry_run
allowed_targets
commands[].kind
commands[].argv
blocked_by
warnings
expires_at
```

安全规则：

- 计划只保存 argv array。
- apply 前重新校验 service、action、driver、compose file、TTL 和 allowlist。
- real apply 必须显式 `--confirm`。
- dry-run 不执行。
- apply 使用 service lock、timeout、输出长度限制和 redaction。
- operation history 写入本地日志；数据库可达时同步写入 runtime operation/audit tables。

## Hotplug 结论

- L0 Metadata Hotplug：完成。
- L1 Route/Menu/Topology/Permission Hotplug：基本完成。
- L2 Service Runtime Foundation + Controlled Apply：foundation 完成，apply 只通过 `ojosctl`/operator。
- L3 Dynamic Frontend Extension：未完成。
- L4 Full Module Hotplug：未完成。
