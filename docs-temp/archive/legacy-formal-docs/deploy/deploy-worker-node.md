# Worker Node 部署

> 文档状态：需要真实多机验收
> 适用范围：Judge Worker 部署与运行验收
> 最后更新：2026-06-27

## 1. 文档目的

本文档说明如何在独立机器部署 `judge-worker`，使多台 worker 通过 Worker Link 共同评测。worker node 是不可信用户代码的执行边界，部署时必须同时关注资源限制、安全隔离和任务 lease。

## 2. 适用范围

适用于第二台、第三台 Linux worker 机器部署，也适用于单机双 worker 模拟。Windows 环境不能真实验证 nsjail、cgroup v2 和 fork bomb 防护。

## 3. 当前实现

当前 worker 位于 `services/judge-worker`，配置文件为 `services/judge-worker/config/languages.yaml`，独立部署 compose 位于 `deploy/worker/docker-compose.yml`，环境模板位于 `deploy/worker/.env.example`。

worker 通过 Gateway 访问：

- `POST /api/judge/worker/register`
- `POST /api/judge/worker/heartbeat`
- `POST /api/judge/worker/tasks/claim`
- artifact download 和 result upload API

## 4. 禁止事项

worker node 不允许：

- 直连 PostgreSQL。
- 直连 Redis。
- 挂载 Control Plane 的 `storage/problems` 或 `storage/submissions`。
- 使用主服务本地路径作为题目包或源码来源。
- 对公网暴露自己的服务端口。

## 5. 环境要求

Linux host 必须具备：

```bash
test -f /sys/fs/cgroup/cgroup.controllers
cat /sys/fs/cgroup/cgroup.controllers
docker version
```

预期：cgroup v2 可用，包含 `memory` 和 `pids` controller，Docker daemon 正常运行，nsjail 在容器内可用。

## 6. 配置说明

复制环境模板：

```powershell
Copy-Item deploy\worker\.env.example deploy\worker\.env
```

必须配置：

| 变量 | 说明 |
| --- | --- |
| `OJOS_WORKER_ID` | worker 唯一标识，多台机器不能重复 |
| `OJOS_WORKER_NAME` | 管理页面显示名 |
| `OJOS_CONTROL_PLANE_URL` | Gateway 地址 |
| `OJOS_WORKER_TOKEN` | Worker API token |
| `OJOS_MAX_CONCURRENCY` | 本机最大并发 |
| `OJOS_WORK_DIR` | worker 临时工作目录 |
| `OJOS_SUPPORTED_LANGUAGES` | 支持语言列表 |
| `OJOS_HEARTBEAT_INTERVAL` | 心跳间隔 |
| `OJOS_TASK_LEASE_TTL` | task lease TTL |
| `OJOS_ARTIFACT_CACHE_DIR` | artifact 缓存目录 |
| `OJOS_LOG_LEVEL` | 日志级别 |

## 7. 启动命令

```powershell
docker compose --env-file deploy\worker\.env -f deploy\worker\docker-compose.yml up -d --build
```

Docker daemon 不可用时，只能执行 compose config 或静态构建检查，不能声明 worker runtime 已通过。

## 8. 注册验证

登录管理员前端，打开 `/admin/judge`。预期看到 worker_id、hostname、version、supported_languages、max_concurrency、running_count、last_seen 和 ONLINE 状态。

## 9. 双 worker 模拟

在同一台 Linux 主机可复制两份 `.env`，设置不同 `OJOS_WORKER_ID`、`OJOS_WORK_DIR` 和 `OJOS_ARTIFACT_CACHE_DIR`。每个 worker 设置 `OJOS_MAX_CONCURRENCY=1`，提交多份任务后应看到两个 worker 都参与评测。

Control Plane compose 也可以用于本机双 worker 验收：

```bash
docker compose --env-file .env -f deploy/compose/docker-compose.yml up -d --scale judge-worker=2 judge-worker
```

2026-06-27 已在 `Ubuntu-24.04-OJOS` WSL2 Linux 环境完成本机双 worker 模拟验收，两个 worker 均 ONLINE，worker_id 不冲突，四语言矩阵任务分布到两个 worker，未发现重复 claim。

## 10. 下线与升级

滚动升级流程：

1. 在 `/admin/judge` 对 worker 执行 drain。
2. 等 running task 完成。
3. 停止旧 worker 容器。
4. 部署新镜像。
5. 确认 heartbeat 恢复。
6. 检查没有永久 `JUDGING`。

## 11. 失联恢复

worker 失联后，`judge_tasks.lease_expires_at` 过期，Control Plane 恢复任务。新 worker claim 后旧 worker 上传 result 必须因 lease_version 过期而失败。

2026-06-27 验收中，worker A claim 长运行任务后被停止；lease 过期后 worker B 重新 claim，`lease_version` 从 1 增至 2；旧 lease result 上传返回 HTTP 400，最终 submission 未被旧结果覆盖。

## 12. 安全边界

worker token 必须通过环境变量注入，不写入镜像。worker node 不保存数据库凭据，不暴露 Redis，不接收内部路径，只处理带 digest 的 artifact。

## 13. 验收方式

- worker token 错误时注册失败。
- 两个 worker 同时在线。
- 连续提交任务分布到两个 worker。
- 停掉 worker A 后任务由 worker B 恢复。
- `/admin/judge` 能看到 worker 状态变化。
- stale lease result 被拒绝。
- Redis Stream 仅作为 signal history，PostgreSQL `judge_tasks` 是任务所有权事实源。

本次验收未覆盖第二台真实机器。真实多机部署还需要在独立 Linux worker node 上重复执行注册、claim、crash recovery、跨主机网络抖动、断网恢复和时钟漂移检查。

## 14. 常见问题

- worker OFFLINE：检查 token、Control Plane URL 和网络。
- claim 为空：检查语言能力、slot 和 pending task。
- MLE/OLE 不正确：检查 cgroup v2 和 nsjail 权限。
- drain 后仍接任务：检查 admin API 和 worker heartbeat 是否同步。

## 15. 相关文档

- [Worker Link 协议](../architecture/worker-link-protocol.md)
- [资源限制](../judge/judge-resource-limits.md)
- [Linux 运行验收](../e2e/e2e-linux-runtime.md)
