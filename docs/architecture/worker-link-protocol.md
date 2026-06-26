# Worker Link 协议

> 文档状态：部分实现
> 适用范围：架构设计 / Judge Worker 部署 / 安全
> 最后更新：2026-06-26

## 1. 文档目的

本文档定义 OJOS Control Plane 与 `judge-worker` 之间的通信协议。Worker Link 的目标是让 worker 可以部署在多台独立机器上，通过出站请求参与评测，而不依赖主服务本地磁盘、PostgreSQL 或 Redis 网络访问。

## 2. 适用范围

本文档适用于维护 `services/judge-api`、`services/judge-worker`、`services/gateway`、worker compose、admin judge 页面和 E2E 验收脚本的开发者。部署人员也应阅读本文档，确认 worker 节点没有被错误配置为直连数据库或挂载 Control Plane storage。

## 3. 当前实现

当前 Worker Link 提供以下能力：

- worker 使用 `X-OJOS-Worker-Token` 注册。
- worker 定期 heartbeat，上报并发、语言和运行任务。
- worker 按 available slots claim task。
- `Judge API` 使用 PostgreSQL `judge_tasks` 进行原子 claim。
- worker 通过 artifact API 下载源码和题目包。
- worker 上传 result、case 摘要和截断日志。
- `lease_version` 用于拒绝旧 lease result。

相关路径：

- `services/judge-api/internal/logic/workerclaimtaskslogic.go`
- `services/judge-api/internal/logic/workersubmitresultlogic.go`
- `services/judge-api/internal/repository/worker.go`
- `services/judge-worker/src/worker_link.rs`
- `services/judge-api/internal/middleware/workerauthmiddleware.go`

## 4. 禁止的旧模式

worker 不允许：

- 直连 PostgreSQL。
- 直连 Redis。
- 挂载 Control Plane 的 `storage/problems` 或 `storage/submissions`。
- 接收服务器本地路径。
- 对公网暴露 worker HTTP 服务。

这些限制是多机部署的前提。worker 只应通过 Gateway 访问 Worker API。

## 5. 鉴权模型

worker 请求必须携带：

```http
X-OJOS-Worker-Token: <token>
```

请求进入 `Gateway` 后，Gateway 到 `Judge API` 的内部请求还会被 HMAC 签名。`Judge API` 必须同时校验：

1. worker token 是否正确。
2. Gateway 内部 HMAC 是否有效。
3. 当前 task lease 是否属于该 worker。

worker token 不能替代 lease 校验。即使 token 正确，旧 `lease_version` 的 result upload 也必须被拒绝。

## 6. 协议端点

| API | 调用方 | 作用 |
| --- | --- | --- |
| `POST /api/judge/worker/register` | worker | 注册 worker 身份、版本、语言、能力和并发 |
| `POST /api/judge/worker/heartbeat` | worker | 更新 worker last_seen、running tasks、slots |
| `POST /api/judge/worker/tasks/claim` | worker | 领取任务并获得 lease 与 artifact 描述 |
| `POST /api/judge/worker/tasks/:task_id/heartbeat` | worker | 刷新 task lease，防止 stale recovery 误回收 |
| `POST /api/judge/worker/tasks/:task_id/result` | worker | 上传最终结果、case 摘要和截断日志 |
| `POST /api/judge/worker/tasks/:task_id/fail` | worker | 上报系统失败或用户失败 |
| `GET /api/judge/worker/artifacts/submissions/:id/source` | worker | 下载提交源码 |
| `GET /api/judge/worker/artifacts/problems/:id/package` | worker | 下载题目包 |

## 7. 请求与响应示例

register 请求示例：

```json
{
  "worker_id": "worker-a",
  "worker_name": "linux-worker-a",
  "hostname": "worker-a-host",
  "version": "0.1.0",
  "capabilities": ["nsjail", "cgroup_v2"],
  "languages": ["cpp17", "c11", "python3", "java17"],
  "max_concurrency": 2
}
```

claim 响应示例：

```json
{
  "tasks": [
    {
      "task_id": "task-100-1",
      "submission_id": 100,
      "worker_id": "worker-a",
      "lease_version": 3,
      "lease_expires_at": "2026-06-26T08:30:00Z",
      "attempt": 1,
      "language": "cpp17",
      "source": {
        "url": "/api/judge/worker/artifacts/submissions/100/source",
        "sha256": "<digest>",
        "size_bytes": 1024
      },
      "problem_package": {
        "url": "/api/judge/worker/artifacts/problems/1/package",
        "sha256": "<digest>",
        "size_bytes": 4096
      }
    }
  ]
}
```

result upload 必须携带当前 `task_id`、`worker_id`、`lease_version` 和结果摘要。服务端以数据库中的当前 lease 为准。

## 8. Lease 字段

- `worker_id`：当前任务归属 worker。
- `task_id`：一次 task lease 的标识。
- `lease_version`：版本号，用于拒绝旧结果。
- `lease_expires_at`：过期时间，驱动 stale recovery。
- `attempt`：尝试次数，用于重试上限和审计。
- `heartbeat_at`：任务心跳时间，用于观测和排查。

## 9. 失败恢复

worker 崩溃或网络断开后，`judge_tasks.lease_expires_at` 到期。调度逻辑恢复任务，递增 lease 或生成新 task。旧 worker 恢复后如果上传旧 result，`Judge API` 会发现 `lease_version` 不匹配并拒绝，避免覆盖新 worker 的结果。

## 10. Redis Streams 的角色

Redis Streams 是 signal history，用于唤醒、观测和长度控制，不是任务所有权来源。任务是否可 claim、当前 worker owner、lease 是否有效，都以 PostgreSQL `judge_tasks` 为准。

## 11. 配置说明

worker 侧关键环境变量：

- `OJOS_WORKER_ID`
- `OJOS_WORKER_NAME`
- `OJOS_CONTROL_PLANE_URL`
- `OJOS_WORKER_TOKEN`
- `OJOS_MAX_CONCURRENCY`
- `OJOS_SUPPORTED_LANGUAGES`
- `OJOS_HEARTBEAT_INTERVAL`
- `OJOS_TASK_LEASE_TTL`

Control Plane 必须配置与 worker 一致的 `OJOS_WORKER_TOKEN`。

## 12. 安全边界

Worker API 是 worker-only。普通用户、管理员用户和浏览器前端都不应直接调用 Worker API。Artifact 下载必须校验 task lease，不能只校验 worker token。

## 13. 验收方式

- worker token 错误时 register 失败。
- 两个 worker 同时在线时，单个 submission 只被一个 worker claim。
- 停止 worker A 后，lease 过期，worker B 能接手。
- 旧 `lease_version` result upload 被拒绝。
- worker 节点无需 PostgreSQL、Redis 凭据。

## 14. 常见问题

- worker 注册 401：检查 token、Gateway 转发和 Judge API middleware。
- claim 返回空：检查 available slots、language capability 和 pending task。
- result upload 409：通常是旧 lease 或重复上传，需要查看 `lease_version`。
- artifact 下载失败：检查 task ownership、digest 和 artifact storage。

## 15. 相关文档

- [Worker API](../api/worker-api.md)
- [Judge Worker 集群](../judge/judge-worker-cluster.md)
- [Worker Token](../security/worker-token.md)
- [Storage 与 Artifact 模型](storage-artifact-model.md)
