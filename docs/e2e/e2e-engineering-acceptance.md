# E2E 工程验收总入口

> 文档状态：当前实现
> 适用范围：E2E 验收 / 发布前检查 / 运维
> 最后更新：2026-06-26

## 2026-06-27 Admin Health 验收补充

Docker API 验收必须确认 `GET /api/admin/health` 在 Control Plane 正常启动时整体状态为 `ok`，且 `judge` 子项为 `ok`。`judge` 子项的健康检查路径是 compose 内部 `judge-api` 服务的 `GET /health`，不是 Gateway public `/api/judge/*` 路由。

如果 Admin Health 返回 `degraded`，必须区分真实子项异常和探测路径错误。`judge-api` 正常运行但 health 子项返回 404 属于实现或配置缺陷，不能作为外部环境阻塞处理；修复后需重新运行 `scripts\e2e-api.ps1` 并确认摘要包含 `admin_health_status=ok`、`admin_health_judge_status=ok`、`failed=0`、`path_leaks=0`。

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
## 2026-06-26 Docker API 运行时验收补充

本仓库现在区分四类验收入口：

- 静态验证：`powershell -NoProfile -File scripts\verify-static.ps1 -SkipDockerBuild`，只覆盖格式、构建、单元测试、文档扫描、compose config 和前端 build 等静态事项。
- Docker API 验证：先真实执行 `docker compose --env-file .env -f deploy\compose\docker-compose.yml up -d --build`，再执行 `powershell -NoProfile -File scripts\e2e-api.ps1 -BaseUrl http://localhost:8080/api -AdminUsername admin1 -AdminPassword admin123 -UserUsername user1 -UserPassword user123 -WorkerToken $env:OJOS_WORKER_TOKEN`。
- Linux 资源限制验收：`scripts/e2e-linux.sh`，用于 nsjail、cgroup v2、TLE/MLE/OLE 等 Linux worker runtime 行为。
- 多机 worker 验收：需要 Linux/Docker/多机网络环境，验证远程 worker、并发、lease 恢复和故障切换。

静态验证结果不得替代 API 验收；Docker daemon 不可用时必须记录为“无法执行 Docker API 验收”。
# 2026-06-27 Module Installer E2E 覆盖

`scripts/e2e-api.ps1` 已扩展 Module Installer runtime smoke：

- `/api/admin/modules/discover`
- `/api/admin/modules/validate`
- `/api/admin/modules/plan`
- `/api/admin/modules/install` dry-run / apply demo module
- `/api/admin/modules/:id/enable`
- `/api/admin/modules/:id/disable`
- `/api/admin/modules/:id/upgrade-plan`
- `/api/admin/modules/:id/rollback-plan`
- `/api/admin/modules/:id/uninstall-dry-run`
- `/api/admin/modules/:id/health`
- `/api/admin/modules/:id/operations`
- 普通用户 403、无 token 401
- `ojos.judge-core` disable 被拒绝
- internal service 不通过 compose host port 暴露

验收必须保持 `failed=0`、`path_leaks=0`、`admin_health_status=ok`、`admin_health_judge_status=ok`。

Installer hardening 后，静态验收还必须覆盖：

- root Rust workspace `cargo fmt --check`、`cargo check`、`cargo test`。
- `ojosctl --version`。
- `ojosctl module doctor`。
- `ojosctl module validate modules/demo-module/module.yaml`。
- `ojosctl module package modules/demo-module -o .tmp/agent/scratch/verify-static-demo.ojosmod`。
- `ojosctl module verify` 和 `ojosctl module inspect`。
- compose config 包含 `module-installer` internal service、`read_only: true`、`no-new-privileges:true`、`cap_drop: ALL`、只读 `modules/` 挂载和 lock TTL 配置。
- module-installer Dockerfile 使用 builder/runtime 多阶段，最终 runtime image 不得是 `rust:*`。

## Module SDK Compatibility Harness

执行：

```powershell
powershell -NoProfile -File scripts\e2e-module-compat.ps1 `
  -BaseUrl http://localhost:8080/api `
  -AdminUsername admin1 `
  -AdminPassword admin123 `
  -UserUsername user1 `
  -UserPassword user123
```

该 harness 验证 scaffold、package、install、enable、Runtime Snapshot、route/menu/topology/permission/service contribution、metadata service plan blocking、disable、include-disabled inspection、uninstall dry-run、权限拒绝和 path leak checks。

## Kernel Baseline Acceptance

执行：

```powershell
powershell -NoProfile -File scripts\acceptance-kernel.ps1 -SkipDockerBuild
```

统一 Kernel 验收入口会调用 static verification、API e2e、module compatibility 和 `ojosctl` smoke checks，并写出以下 summary：

```text
static_failed
api_failed
compat_failed
path_leaks
admin_health_status
admin_health_judge_status
module_compat
controlled_apply
overall_status
```

Controlled apply is skipped by default. It only runs when `-RunControlledApply` is passed.
