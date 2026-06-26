# E2E 工程验收总入口

> 文档状态：当前实现
> 适用范围：E2E 验收 / 发布前检查 / 运维
> 最后更新：2026-06-26

## 1. 文档目的

本文档定义 OJOS 从静态构建到运行验收的完整检查入口。它强调一个原则：只有真实执行命令并得到预期结果，才可以记录为通过；Docker daemon、Linux cgroup v2、nsjail 或多机环境缺失时，只能记录为外部阻塞。

## 2. 适用范围

适用于发布前检查、功能验收、部署演练和故障恢复演练。Windows 开发环境可执行静态验收；Linux 主机负责真实 worker runtime 验收。

## 3. 当前实现

当前仓库提供：

- `scripts/verify-static.ps1`：Go/Rust/Frontend/compose/安全扫描的静态验收。
- `scripts/e2e-linux.sh`：Linux runtime 验收入口，依赖 Docker、cgroup v2、nsjail 和有效 worker token。
- 前端页面覆盖登录、题目、提交、权限、健康检查和 worker 管理。
- Worker Link 支持 register、heartbeat、claim、task heartbeat、result upload 和 fail upload。

## 4. 静态验收

命令：

```powershell
powershell -NoProfile -File scripts\verify-static.ps1 -SkipDockerBuild
```

预期结果：

- `gofmt` 无待格式化文件。
- Go build/test 通过。
- `cargo fmt --check` 与 `cargo check` 通过。
- `npm run build` 通过。
- `docker compose config` 通过。
- frontend direct-call、public schema 内部路径、危险部署配置扫描通过。

失败排查：进入失败服务目录单独执行对应命令。例如前端失败时执行：

```powershell
cd frontend
npm run build
```

## 5. Docker build 验收

命令：

```powershell
powershell -NoProfile -File scripts\verify-static.ps1
```

当前环境限制：该命令需要 Docker daemon。daemon 不可用时，不影响文档重构和静态验证，但不能把 Docker build 写成通过。

## 6. Linux runtime 验收

命令：

```bash
OJOS_WORKER_TOKEN=<token> bash scripts/e2e-linux.sh
```

验收前必须确认：

```bash
test -f /sys/fs/cgroup/cgroup.controllers
cat /sys/fs/cgroup/cgroup.controllers
docker version
```

预期包含 `memory` 和 `pids` controller，Docker daemon 可用，nsjail 可运行。

## 7. 业务流程验收

运行验收必须覆盖：

| 步骤 | 预期结果 | 失败排查 |
| --- | --- | --- |
| migrations | 数据库结构与代码匹配 | 检查 `deploy/migrations/` 和服务日志 |
| admin bootstrap | 管理员可登录 | 检查 Auth API 和 PostgreSQL |
| register/login | 普通用户可注册登录 | 检查 JWT、用户表和前端 auth store |
| create problem | 管理员可创建题目 | 检查 Problem API 权限 |
| package validate | A+B 题目包通过校验 | 检查 `problem.yaml` 和 `tests/cases.yaml` |
| submit AC/WA/CE/RE/TLE/MLE/OLE | 状态符合预期 | 检查 worker 日志、资源限制和 case 结果 |
| worker register | worker 出现在 `/admin/judge` | 检查 token、Gateway、Judge API |
| two worker concurrency | 两个 worker 都领取任务 | 检查 `OJOS_MAX_CONCURRENCY` 和 claim 逻辑 |
| worker crash recovery | 过期 lease 被恢复 | 检查 `judge_tasks.lease_expires_at` |
| permission grant/remove | 权限变化真实生效 | 检查权限审计和后端权限判断 |
| frontend flow | 页面不白屏且使用真实 API | 检查 Network、API client 和 router guard |

## 8. Redis signal history 验收

Redis Streams 只作为 signal history，不是任务所有权来源。验收时需要检查 stream 长度受控、pending 指标可观测、PostgreSQL `judge_tasks` 仍是事实源。

## 9. 常见问题

- 长时间 `JUDGING`：检查 worker heartbeat 和 stale recovery。
- MLE 判成 RE：检查 cgroup v2 `memory.events`。
- OLE 不出现：检查 stdout/stderr 文件大小限制。
- 普通用户能访问 admin：检查后端权限中间件。
- worker 直连 DB/Redis：这是错误部署，应改为 Worker Link。

## 10. 相关文档

- [Linux 运行验收](e2e-linux-runtime.md)
- [E2E 静态检查](e2e-static-checks.md)
- [Judge E2E 用例](../judge/judge-e2e-cases.md)
- [Worker Link 协议](../architecture/worker-link-protocol.md)
