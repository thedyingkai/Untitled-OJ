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
