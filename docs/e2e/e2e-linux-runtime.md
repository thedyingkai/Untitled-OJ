# Linux 运行验收

> 文档状态：当前实现，WSL2 Linux 已验收，真实多机待补充
> 适用范围：E2E 验收 / Judge Worker / 多 worker 恢复
> 最后更新：2026-06-27

## 1. 文档目的

本文档说明必须在 Linux 环境执行的真实运行验收。它覆盖 Docker、nsjail、cgroup v2、worker 注册、资源限制、双 worker 并发、worker crash recovery、stale lease 拒绝、Redis signal history 和权限/路径泄露复扫。

静态验证、Docker API 验收和 Linux Judge Runtime 验收是三类不同验收：

| 验收 | 命令 | 目的 |
| --- | --- | --- |
| 静态验证 | `powershell -NoProfile -File scripts\verify-static.ps1 -SkipDockerBuild` | 检查代码、文档、前端构建和静态规则 |
| Docker API 验收 | `powershell -NoProfile -File scripts\e2e-api.ps1 ...` | 通过 Gateway 验证 Control Plane API、权限、HMAC、DB、Redis、storage |
| Linux Runtime 验收 | `bash scripts/e2e-linux.sh` | 验证真实 Linux sandbox、nsjail、cgroup v2、资源限制和 worker lease 行为 |

## 2. 已执行环境

2026-06-27 已在 Windows 开发机的 D 盘 WSL2 环境完成一次 Linux Judge Runtime 验收。下表记录环境类别和版本，不作为部署默认路径：

| 项目 | 实际结果 |
| --- | --- |
| WSL 发行版 | `Ubuntu-24.04-OJOS` |
| WSL 安装位置 | D 盘 WSL2 环境 |
| 项目路径 | Windows 工作区经 WSL mount 访问 |
| Linux | Ubuntu 24.04 LTS |
| Kernel | WSL2 `5.15.167.4-microsoft-standard-WSL2` |
| Docker | `29.6.1` |
| Docker Compose | `v5.2.0` |
| cgroup v2 | `/sys/fs/cgroup/cgroup.controllers` 存在 |
| controllers | `cpuset cpu io memory hugetlb pids rdma misc` |
| worker 数量 | 本机 `--scale judge-worker=2` 双 worker 模拟 |

本次没有使用第二台真实机器，因此不能写成“真实多机 worker 验收通过”；结论仅覆盖 WSL2 Linux 本机双 worker 模拟。

## 3. 执行命令

```bash
test -f /sys/fs/cgroup/cgroup.controllers
cat /sys/fs/cgroup/cgroup.controllers
docker version
docker compose version
docker compose --env-file .env -f deploy/compose/docker-compose.yml up -d --build
bash scripts/e2e-linux.sh
```

脚本会自动执行 API bootstrap、四语言状态矩阵、fork bomb、残留进程检查、双 worker 分布、worker crash recovery、stale lease result 拒绝、Redis signal history、Admin Health/Judge、权限复扫和路径泄露扫描。

## 4. 2026-06-27 验收结果

| 项目 | 结果 |
| --- | --- |
| Control Plane build/up | 通过 |
| API smoke | 通过 |
| Admin Health | `ok` |
| Judge health 子项 | `ok` |
| 状态矩阵 | 28/28 通过 |
| 覆盖语言 | `cpp17`、`c11`、`python3`、`java17` |
| 覆盖状态 | AC/WA/CE/RE/TLE/MLE/OLE |
| `memory_kb` | 非 0 记录 24 条，不恒为 0 |
| MLE | 正确为 `MEMORY_LIMIT_EXCEEDED` |
| OLE | 正确为 `OUTPUT_LIMIT_EXCEEDED` |
| TLE | 正确为 `TIME_LIMIT_EXCEEDED` |
| fork bomb | 被限制，未拖垮 host |
| TLE 后残留进程 | 未发现残留 `nsjail` 或 `/work/main` |
| 双 worker | 通过，两个 worker 均参与矩阵任务 |
| worker_id 冲突 | 未发现 |
| worker crash recovery | 通过 |
| stale lease result | HTTP 400 拒绝，未覆盖最终结果 |
| Redis signal history | 通过 |
| 路径泄露扫描 | `path_leaks=0` |
| 权限复扫 | `permission_failures=0` |

运行报告和日志只保存在 `.tmp/agent/`，不得提交：

- `.tmp/agent/reports/linux-runtime/summary.json`
- `.tmp/agent/reports/linux-runtime/status-matrix.tsv`
- `.tmp/agent/reports/linux-runtime/worker-crash-recovery.md`
- `.tmp/agent/reports/linux-runtime/redis-signal-history.md`
- `.tmp/agent/logs/linux-runtime/e2e-linux.log`

## 5. 验收边界

本次通过说明 A/Judge Core 在 WSL2 Linux + Docker + cgroup v2 + nsjail 环境下完成了核心运行时验收，包含本机双 worker 模拟和 worker crash recovery。

仍未执行的项目：

- 第二台真实 Linux worker node。
- 跨主机网络抖动、断网和时钟漂移。
- 长时间 soak test。
- 生产级最小 capability hardening 复核。

## 6. 安全边界

脚本不应输出 secret。worker 不直连 DB/Redis，不挂载 Control Plane storage，不向 public API 泄露 `code_path`、`result_path`、`package_dir`、`stdout_path`、`stderr_path`、`checker_log_path`、`/work`、`/sys/fs/cgroup`、`storage/problems` 或 `storage/submissions`。

## 7. 常见问题

- cgroup 缺 controller：启用 cgroup v2，并确认 `memory`、`pids` 存在。
- nsjail 失败：检查 worker 镜像、capability 和 `/sys/fs/cgroup` mount。
- MLE 变成 TLE/RE：检查子进程是否加入 case cgroup，以及 `memory.events` 是否可读。
- OLE 变成 SYSTEM_ERROR：检查 stdout/stderr 文件大小限制与错误分类。
- 永久 `JUDGING`：检查 task lease TTL、worker heartbeat 和 stale recovery。
- 旧结果覆盖：检查 result upload 是否校验 `worker_id` 与 `lease_version`。

## 8. 相关文档

- [资源限制](../judge/judge-resource-limits.md)
- [Worker 集群](../judge/judge-worker-cluster.md)
- [Worker Node 部署](../deploy/deploy-worker-node.md)
- [工程验收总入口](e2e-engineering-acceptance.md)
