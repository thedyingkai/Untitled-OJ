# Module Runtime

> 文档状态：当前实现，Phase 1 runtime snapshot
> 适用范围：Kernel / Gateway / Web Shell / 模块作者
> 最后更新：2026-06-27

Module Runtime 是 OJOS Kernel 能力。它读取 Module Registry，计算当前 enabled modules，并导出 runtime snapshot。

## Runtime Snapshot

```json
{
  "modules": [],
  "permissions": [],
  "menus": [],
  "frontend_routes": [],
  "gateway_routes": [],
  "services": [],
  "workers": [],
  "health_checks": [],
  "topology": {
    "nodes": [],
    "edges": []
  }
}
```

## Phase 1 行为

- Gateway 新增 `/api/admin/modules/runtime-snapshot`。
- `/api/admin/modules/topology` 从 runtime snapshot 派生，保持旧响应结构兼容。
- Runtime snapshot 聚合 module nodes、permissions、menus、frontend routes、gateway routes、components、health checks 和 topology edges。
- Frontend topology 页面读取 runtime snapshot。

## 未来路线

- L0：metadata hotplug 已作为当前目标。
- L1：Gateway route hotplug 逐步从 snapshot 读取。
- L2：service/worker hotplug 通过受控 runtime driver 和 operator plan 实现，不暴露 Docker socket。
- L3：frontend contribution hotplug 必须依赖签名 package、CSP、sandbox iframe 或 web component，不执行不可信动态 JS。
- L4：完整安装、部署、路由、权限、frontend、health、rollback 自动化。

## Kernel Runtime Wiring v1

Runtime Snapshot is the running-state source of truth for module contributions. `GET /api/admin/modules/runtime-snapshot` returns only ENABLED module contributions by default. `GET /api/admin/modules/runtime-snapshot?include_disabled=true` returns registry-visible disabled module entries for admin inspection.

Snapshot v1 includes:

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

Runtime aggregation rules:

- Active snapshot filters by enabled modules and does not expose disabled module contributions as active runtime surface.
- `include_disabled=true` is admin-only and is for registry inspection, installer review, and debugging.
- Every contribution carries `module_id` so operators can trace source ownership.
- Manifest-only metadata such as roles, storage buckets, events, admin panels, scheduled jobs and topology contributions is derived from the stored manifest when no dedicated registry table exists yet.
- Snapshot responses must not include secrets, tokens, DB connection strings, local absolute paths, Docker socket paths, or package internals.

Gateway route hotplug L1 is implemented for enabled module routes. Gateway exposes `GET /api/admin/modules/runtime/routes` and `POST /api/admin/modules/runtime/reload` to rebuild and validate the runtime route table from registry data. Dynamic proxy matching uses the route table after core static routes and before compatibility fallback routes. The runtime table validates reserved prefixes, duplicate prefixes, overlapping prefixes, unknown services and auth modes. Manifest route declarations may reference only a `service_id`; upstream URLs come from Gateway trusted service configuration, not from module manifests.

Dynamic proxy security rules:

- Core static routes keep priority for `/api/auth` and `/api/judge/worker`.
- Reserved prefixes cannot be claimed by modules: `/api/auth`, `/api/admin/modules`, `/api/admin/health`, `/api/health`, `/api/internal`, `/api/judge/worker`.
- `service_id` must exist in Gateway trusted service configuration.
- Admin route table responses hide `upstream_base` by default.
- Raw `Authorization` is not forwarded through dynamic routes. Gateway forwards sanitized actor headers and internal HMAC headers.
- `public`, `user`, `admin`, `worker` and `internal` auth modes are explicit. `worker` and `internal` are not public dynamic proxy surfaces.

Topology is generated from Runtime Snapshot. It contains module nodes, dependency edges, service/worker/component nodes, gateway route nodes, frontend menu/route nodes, health nodes, and manifest-declared topology nodes/edges. Admin UI only renders the snapshot; it should not invent topology for future modules.

Current hotplug conclusion:

- L0 Metadata hotplug: implemented for registry/snapshot/menu/permission/topology/health contribution display.
- L1 Gateway route hotplug: runtime route table, reload, trusted service map, reserved prefix protection and dynamic proxy for enabled routes are implemented. Service start/stop remains out of scope.
- L2 Service hotplug: runtime driver contract remains future work.
- L3 Frontend contribution hotplug: metadata display only. OJOS still does not execute untrusted JS or dynamic frontend bundles.
- L4 Full module hotplug: future work.
