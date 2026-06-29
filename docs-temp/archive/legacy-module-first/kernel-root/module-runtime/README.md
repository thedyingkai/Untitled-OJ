# Kernel 模块运行时

本目录是 OJOS Kernel 模块运行时的项目结构边界。当前可执行 Go 兼容实现仍位于 `services/gateway/internal/kernel/moduleruntime`，因为 Gateway 仍是公开管理 API 进程；一次性移动所有 Go 服务会增加运行风险。

Runtime 契约归属本目录，职责包括：

- 读取 module registry tables。
- 计算 enabled modules。
- 导出 Runtime Snapshot。
- 聚合 permissions、menus、frontend routes、gateway routes、components、services、workers、health checks 和 topology。
- 为 controlled service/worker start/stop plans 提供 runtime driver interfaces。

当前 API 表面：

```text
GET /api/admin/modules/runtime-snapshot
GET /api/admin/modules/topology
```

当前热插拔等级为 L0 metadata hotplug 和 L1 route-snapshot 基础。L2 service runtime foundation 已具备 plan/controlled apply 基础；L3 dynamic frontend bundle 与 L4 完整模块热插拔自动化未完成。OJOS v0.1.0 不执行不可信脚本，也不加载不可信 dynamic frontend bundles。
