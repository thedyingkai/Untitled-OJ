# Service-first 迁移计划

当前兼容路径到目标路径：

| 当前兼容路径 | 目标路径 |
| --- | --- |
| `kernel/installer/core` | `installer/core` |
| `kernel/installer/service` | `runtime/manager` |
| `kernel/installer/cli` | `installer/cli` |
| `kernel/installer/tui` | `installer/tui` |
| `modules/*/module.yaml` | `services/*/service.yaml` |
| `services/gateway/internal/kernel/moduleruntime` | `runtime/manager` |

本轮已完成 Service 契约、Set 预设、Runtime 主表、CLI 主命令、Web Shell 边界和文档主线。

剩余迁移包括后端 API 全面改名、旧 `module_*` 表数据迁移、Native GUI 完整实现和 Non-root Agent 远程执行通道。
