# Linux 运行验收

> 文档状态：需要运行验收
> 适用范围：E2E 验收 / Judge Worker / 多机部署
> 最后更新：2026-06-26

## 1. 文档目的

本文档说明必须在 Linux 环境执行的真实运行验收。它覆盖 Docker、nsjail、cgroup v2、worker 注册、资源限制和多 worker 恢复。

## 2. 适用范围

适用于预发环境、Linux 本机双 worker 模拟和真实多机 worker 验收。Windows 静态环境不能替代本文档。

## 3. 当前实现

运行脚本为 `scripts/e2e-linux.sh`。它需要可访问的 Control Plane、有效 `OJOS_WORKER_TOKEN`、Docker daemon、curl、jq 和 worker runtime 环境。

## 4. 目标设计

E2E runtime 应成为发布前必须执行的验收项，覆盖 AC/WA/CE/RE/TLE/MLE/OLE、worker crash recovery 和权限闭环。

## 5. 关键流程

```bash
test -f /sys/fs/cgroup/cgroup.controllers
cat /sys/fs/cgroup/cgroup.controllers
docker version
OJOS_WORKER_TOKEN=<token> bash scripts/e2e-linux.sh
```

## 6. 配置说明

`OJOS_WORKER_TOKEN` 必须与 Control Plane 配置一致。可用 `OJOS_API_BASE` 指定 Gateway API 地址。

## 7. 安全边界

脚本不应输出 secret。worker 不直连 DB/Redis，不挂载 Control Plane storage。

## 8. 验收方式

脚本退出 0 才算通过。预期无重复评测、无永久 `JUDGING`、MLE/OLE 状态正确、旧 lease result 被拒绝。

## 9. 常见问题

- cgroup 缺 controller：启用 cgroup v2。
- nsjail 失败：检查 worker 镜像和 capability。
- worker offline：检查 token 和 Gateway URL。
- 状态不符：查看 worker 和 Judge API 日志。

## 10. 相关文档

- [资源限制](../judge/judge-resource-limits.md)
- [Worker Node 部署](../deploy/deploy-worker-node.md)
## 2026-06-26 验收边界补充

本文件只描述 Linux worker runtime 验收。Docker Control Plane API 验收请使用 `scripts\e2e-api.ps1`，静态验证请使用 `scripts\verify-static.ps1 -SkipDockerBuild`。不要把 Docker API 验收和 Linux nsjail/cgroup 验收混在一起：前者确认 Gateway、数据库、Redis、storage、HMAC、权限和 API 响应；后者确认真实 Linux sandbox、cgroup v2、资源限制和 worker runtime 行为。
