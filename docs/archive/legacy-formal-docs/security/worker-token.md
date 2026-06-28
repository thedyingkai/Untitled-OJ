# Worker Token

> 文档状态：当前实现
> 适用范围：安全 / Worker Link / 部署
> 最后更新：2026-06-26

## 1. 文档目的

本文档说明 `X-OJOS-Worker-Token` 的用途、边界和轮换要求。worker token 是 worker 节点接入 Worker API 的认证凭据。

## 2. 适用范围

适用于部署 worker node、维护 `Judge API` worker middleware、排查 worker 注册失败和制定密钥轮换流程的人员。

## 3. 当前实现

worker 在 register、heartbeat、claim、artifact download、result upload 等请求中携带 `X-OJOS-Worker-Token`。Judge API 验证 token 后，还会校验 task lease。

## 4. 目标设计

后续可支持多 token、worker 分组 token 或 mTLS，但当前实现以共享 worker token 为准。无论认证方式如何变化，task lease 校验都必须保留。

## 5. 关键流程

worker 启动后读取 `OJOS_WORKER_TOKEN`，向 Gateway 发起 register。Gateway 转发到 Judge API，Judge API 校验 token 和内部 HMAC，成功后记录 worker last_seen 和能力。

worker token 的校验顺序应保持稳定：

1. Gateway 接收 worker 请求并按普通外部请求处理。
2. Gateway 转发到 Judge API 时附加内部 HMAC。
3. Judge API 先验证内部 HMAC，再验证 `X-OJOS-Worker-Token`。
4. register/heartbeat 更新 worker 记录。
5. claim/result/fail/task heartbeat 继续校验 `worker_id`、`task_id`、`lease_version` 和 `lease_expires_at`。

也就是说，token 是“准入凭据”，lease 是“任务所有权凭据”。没有 lease 的 worker 不能下载任意 artifact，也不能上传任意 submission 的结果。

## 6. 配置说明

Control Plane 与 worker node 必须配置一致 token。生产环境 token 必须通过环境变量或 secret 管理注入，不写入镜像和 Git。

worker node 需要的最小配置包括：

| 变量 | 说明 |
| --- | --- |
| `OJOS_CONTROL_PLANE_URL` | 指向 Gateway API，不指向内部服务 |
| `OJOS_WORKER_TOKEN` | worker-only 凭据 |
| `OJOS_WORKER_ID` | worker 稳定标识，可由部署系统生成 |
| `OJOS_MAX_CONCURRENCY` | 单 worker 并发数 |
| `OJOS_SUPPORTED_LANGUAGES` | worker 声明支持的语言 |

轮换 token 时，应先更新 Control Plane 可接受的 token，再滚动重启 worker。当前实现如果只支持单 token，就必须安排短维护窗口或通过并行 worker 池完成平滑切换。

## 7. 安全边界

worker token 不授予用户权限，不允许访问 admin 用户 API。artifact 下载、task heartbeat、result upload 必须同时校验当前 lease owner。

## 8. 验收方式

错误 token 注册失败；正确 token 但旧 `lease_version` 的 result upload 失败；worker 不需要 PostgreSQL 或 Redis 凭据。

部署验收应同时检查配置和行为：

```bash
echo "$OJOS_WORKER_TOKEN" | wc -c
```

上面的命令只能用于确认变量存在，不应把 token 打到日志。行为验收包括错误 token 注册失败、正确 token 注册成功、drain 后 worker 不再 claim 新任务、旧 lease 上传结果返回 409。若 worker 需要 DB/Redis 凭据才能启动，说明部署边界错误，应回到 Worker Link 配置排查。

## 9. 常见问题

- 401：token 缺失或不一致。
- 403：token 正确但 task lease 不属于该 worker。
- 轮换后 worker 离线：检查 Control Plane 与 worker 是否同步更新。

## 10. 相关文档

- [Worker Link 协议](../architecture/worker-link-protocol.md)
- [Worker API](../api/worker-api.md)
