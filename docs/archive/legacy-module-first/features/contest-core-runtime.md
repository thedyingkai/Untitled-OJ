# Contest Core Runtime 与拓扑草案

> 文档状态：设计草案，当前未新增 runtime service
> 最后更新：2026-06-27

## Runtime 策略

Contest Core Skeleton 建议引入受信任 `contest-api` service，这样 dynamic route path 才能真实验收。如果排期风险过高，最早的 skeleton 可以先 metadata-only，但那无法验证 API proxy behavior。

推荐 skeleton：

- `contest-api` 作为 managed compose service。
- `contest-scoreboard-worker` 作为 metadata-only future worker。
- `contest-scoreboard-refresh` 作为 disabled metadata scheduled job。

## Trusted Compose 边界

如果实现 `contest-api`，deployment 必须把它加入 trusted compose allowlist。Module manifest 不得定义 arbitrary image、command、script、mount、host path、privileged 或 capability settings。

## Service Lifecycle

| Service | Lifecycle | Runtime | Apply |
| --- | --- | --- | --- |
| `contest-api` | managed | trusted compose | controlled apply through `ojosctl` only |
| `contest-scoreboard-worker` | metadata | metadata | apply blocked |

Gateway/Web 可以请求 plans 和查看 state，但不能 apply runtime plans。

## Health Checks

| Health Check | Target | Required |
| --- | --- | --- |
| `contest-api-health` | `GET /healthz` on `contest-api` | yes if service exists |

如果 `contest-api` 不是 `RUNNING` 或 health 不是 ok，`/api/contest` 应降级或不可用。

## Runtime Route Table

| Prefix | Service ID | Auth Mode | Permission |
| --- | --- | --- | --- |
| `/api/contest` | `contest-api` | `user` | `contest.view` |

## 拓扑草案

```text
ojos.contest-core -> service:contest-api -> route:/api/contest
service:contest-api -> health:contest-api-health
ojos.contest-core -> worker:contest-scoreboard-worker
ojos.contest-core -> module:ojos.judge-core
ojos.contest-core -> storage:contest-exports
```

## Kernel 变更评估

如果 Contest Core Skeleton 保持在 Module Contract v1 内，不应要求 Kernel changes。后续 dynamic frontend bundles、新 event delivery semantics、package trust signatures 或 new runtime drivers 可能需要 Kernel 演进。
