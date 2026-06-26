# Worker API

> 文档状态：部分实现
> 适用范围：Judge Worker / Worker Link / 安全
> 最后更新：2026-06-26

## 1. 文档目的

本文档说明 worker-only API 的路径、鉴权、请求响应、错误和 lease 规则。Worker API 是多机评测的协议边界。

## 2. 适用范围

适用于维护 `services/judge-api` worker handler、`services/judge-worker/src/worker_link.rs`、Gateway 转发和 worker 部署的人员。

## 3. 当前实现

基础路径为 `/api/judge/worker`。worker 使用 `X-OJOS-Worker-Token`，Gateway 到 Judge API 使用内部 HMAC，Judge API 进一步校验 task lease。

## 4. 目标设计

后续可支持 mTLS 或多 token，但当前协议字段 `worker_id`、`task_id`、`lease_version`、`lease_expires_at` 必须保持清晰。

## 5. 关键流程

worker register 后 heartbeat；claim 获取 task lease 和 artifact；评测时刷新 task heartbeat；结束后上传 result 或 fail。

协议顺序必须保持幂等：

1. `register` 上报 worker 元数据、语言能力和并发数。
2. `heartbeat` 周期更新 last_seen、running_count 和 drain 状态。
3. `tasks/claim` 获取一个或多个 task，并获得新的 `lease_version`。
4. `tasks/:task_id/heartbeat` 延长当前 lease。
5. `artifacts/*` 下载源码和题目包 artifact。
6. `tasks/:task_id/result` 上传终态结果。
7. `tasks/:task_id/fail` 上传 worker 侧失败。

如果 worker 崩溃，heartbeat 停止，PostgreSQL 中的 `lease_expires_at` 到期后 stale recovery 重新释放任务。Redis pending 不作为所有权来源。

## 6. 配置说明

worker 侧配置包括 `OJOS_WORKER_ID`、`OJOS_CONTROL_PLANE_URL`、`OJOS_WORKER_TOKEN`、`OJOS_MAX_CONCURRENCY`、`OJOS_SUPPORTED_LANGUAGES`。

## 7. 安全边界

Worker API 不是用户 API。artifact download、task heartbeat、result upload 都必须确认当前 lease owner，不能只看 token。

## 8. API 示例

```http
POST /api/judge/worker/register
X-OJOS-Worker-Token: <token>
```

```json
{
  "worker_id": "worker-a",
  "hostname": "worker-a-host",
  "languages": ["cpp17", "c11"],
  "max_concurrency": 2
}
```

```http
POST /api/judge/worker/tasks/claim
```

返回 `task_id`、`worker_id`、`lease_version`、`lease_expires_at`、source artifact 和 problem package artifact。

```http
POST /api/judge/worker/tasks/:task_id/result
X-OJOS-Worker-Token: <token>
Content-Type: application/json

{"worker_id":"worker-a","lease_version":3,"status":"ACCEPTED","score":100}
```

错误语义：

| 状态码 | 场景 | 处理方式 |
| --- | --- | --- |
| 401 | token 缺失或错误 | worker 停止 claim 并报警 |
| 403 | worker 不拥有 task | 丢弃本地结果并重新同步 |
| 404 | task 不存在或 artifact 不存在 | fail 当前任务并记录 |
| 409 | 旧 lease、重复终态结果 | 不覆盖已有结果 |
| 500 | Control Plane 异常 | worker 指数退避重试 |

## 9. 常见问题

- 401：worker token 错误。
- 403：lease owner 不匹配。
- 409：旧 lease 或重复结果。
- artifact 下载失败：检查 task ownership 和 digest。

## 10. 相关文档

- [Worker Link 协议](../architecture/worker-link-protocol.md)
- [Worker Token](../security/worker-token.md)
