# Contest Core 验收矩阵

> 文档状态：设计草案，不是已实现能力
> 最后更新：2026-06-27

本文定义未来 Contest Core Skeleton 被接受前必须通过的检查。本文件不新增 Contest 验收脚本，也不表示 Contest 已经实现。

| 范围 | 检查 | 预期结果 |
| --- | --- | --- |
| Manifest | `ojosctl module validate modules/contest-core` | Schema v1 通过；没有危险字段。 |
| Package | `ojosctl module package modules/contest-core` | `.ojosmod` 包含 manifest、checksums 和 package metadata。 |
| Verify | `ojosctl module verify <contest-core.ojosmod>` | checksum 校验通过。 |
| Install dry-run | Installer dry-run | 生成安全计划，不写数据库。 |
| Install apply | Installer apply | registry entry 与 contributions 写入。 |
| Enable | 启用模块 | active snapshot 包含 Contest Core contributions。 |
| Disable | 禁用模块 | active routes、menus 和 permissions 被移除。 |
| Runtime snapshot | 管理端 runtime snapshot | 启用后能看到 `ojos.contest-core`。 |
| Runtime routes | Route table | `/api/contest` 绑定到 `contest-api`，或作为 disabled metadata route 展示。 |
| Runtime services | Runtime services | 可见 `contest-api` 状态；metadata workers 可见。 |
| Topology | Topology API/UI | 可见 Contest module、service、route、health、Judge Core dependency edges。 |
| Permissions | Permission registry | `contest.view`、`contest.participate`、`contest.manage` 仅在 active 时可见。 |
| Frontend | Web Shell contribution registry | 菜单和路由通过 contribution 出现，不硬编码 Contest 菜单。 |
| API e2e | Contest API smoke | 无 token 返回 `401`，缺权限返回 `403`，不泄露路径。 |
| Path leaks | e2e path scan | `path_leaks=0`。 |
| Judge Core | Compatibility | Judge Core 保持 enabled/protected，且不标记 GA。 |
| Module compat | Existing harness | sample-hello、demo-module、judge-core 兼容性继续通过。 |

## 必需回归命令

```powershell
powershell -NoProfile -File scripts\acceptance-kernel.ps1 -SkipDockerBuild
powershell -NoProfile -File scripts\verify-static.ps1 -SkipDockerBuild
powershell -NoProfile -File scripts\e2e-api.ps1 -BaseUrl http://localhost:8080/api -AdminUsername admin1 -AdminPassword admin123 -UserUsername user1 -UserPassword user123 -WorkerToken $env:OJOS_WORKER_TOKEN
powershell -NoProfile -File scripts\e2e-module-compat.ps1 -BaseUrl http://localhost:8080/api -AdminUsername admin1 -AdminPassword admin123 -UserUsername user1 -UserPassword user123
```

## Skeleton 最小验收

未来 skeleton 只有在证明 module install/enable/disable、contribution visibility 和 API permission boundaries 全部成立时才可接受。它不得实现完整 scoreboard、clarification、print、balloon、rating 或 remote OJ。
