# Judge Core 模块

> 文档状态：部分实现
> 适用范围：模块规划 / Judge Core / 架构设计
> 最后更新：2026-06-26

## 1. 文档目的

本文档说明当前 Core Judge Platform 如何映射为未来的 `ojos.judge-core` 模块。

## 2. 适用范围

适用于 Judge API、Problem API、Worker Link、前端核心页面和模块系统规划。

## 3. 当前实现

已实现 Gateway 路由、Auth、Problem API、Judge API、Worker Link、Rust `judge-worker`、前端核心页面和管理员页面。`modules/judge-core/module.yaml` 已存在，Gateway 启动时会把 `ojos.judge-core` 以 builtin module 形式写入 Module Registry v0。它当前仍是基础系统的一部分，不是通过 installer 安装。

## 4. 目标设计

后续 installer v0 应读取 `modules/judge-core/module.yaml` 做 validate/install/enable/disable 流程。当前已实现的是只读拓扑登记和后台展示，不执行安装、禁用、升级或回滚。

## 5. 关键流程

Judge Core 覆盖 submit、task claim、artifact download、judge run、result upload、result query 和 admin queue。

当前实现中的主要映射关系：

| 能力 | 当前路径 | 未来模块声明 |
| --- | --- | --- |
| 题目浏览和管理 | `services/problem-api`、`frontend/src/views/problem` | Problem capability |
| 提交和结果 | `services/judge-api`、`frontend/src/views/judge` | Submission/Result capability |
| worker 协议 | `services/judge-api/internal/handler/worker*`、`services/judge-worker/src/worker_link.rs` | Worker Link capability |
| 评测执行 | `services/judge-worker/src` | Judge runtime service |
| 管理面板 | `frontend/src/views/admin` | Admin capability |
| 模块 manifest | `modules/judge-core/module.yaml` | Builtin module manifest |
| 模块拓扑 API | `services/gateway/internal/moduleregistry` | Module Registry v0 |

在模块化前，这些能力作为 Core Judge Platform 一起部署；模块化后，manifest 应把它们声明为一个不可轻易禁用的 Core 模块，供 Contest、Training 等业务模块依赖。

Module Registry v0 中，`ojos.judge-core` 的 topology 组件必须至少包含 `problem-api`、`judge-api`、`judge-worker`、`frontend-routes`、`gateway-routes`、`permissions`、storage bucket 和 health check。`/api/admin/modules/topology` 返回空数组时不能判定为完成，应检查 Gateway 是否执行 bootstrap、`module_*` 表是否有数据，以及路由 `/api/admin/modules/topology` 是否排在 `/api/admin/modules/:id` 之前。

## 6. 配置说明

关键配置包括语言列表、worker token、Redis、PostgreSQL、artifact root、task lease TTL 和 worker concurrency。

## 7. 安全边界

Judge Core 中 Public API、Admin API 和 Worker API 必须分离。worker 不直连 DB/Redis，不接收内部路径。

## 8. 验收方式

静态验证通过后，Module Registry v0 可认为完成；Judge Core 的运行验收仍需要 Linux worker 验证 AC/WA/CE/RE/TLE/MLE/OLE 和多 worker 恢复。因此不能把 A/Judge Core 标记为 GA。

静态验收命令：

```powershell
powershell -NoProfile -File scripts\verify-static.ps1 -SkipDockerBuild
```

模块拓扑静态测试还应覆盖 topology 非空、包含 `ojos.judge-core`、包含关键依赖边、包含关键组件，以及普通用户访问 admin modules 被拒绝。真实 HTTP 验收需要 Control Plane、PostgreSQL、Redis 和 admin token 均可用。

运行验收必须在具备 Docker daemon、Linux cgroup v2 和 nsjail 的环境中执行。验收结果应记录实际 worker id、提交 id、状态、耗时、内存和失败恢复过程。没有执行 Linux 运行验收时，只能标记为“需要运行验收”。

## 9. 常见问题

- core 与 contest 混杂：应拆成模块依赖。
- worker 结果覆盖：检查 `lease_version`。
- 题目包路径泄露：检查 API 转换层。

## 10. 相关文档

- [Worker Link 协议](../architecture/worker-link-protocol.md)
- [模块清单](module-manifest.md)
