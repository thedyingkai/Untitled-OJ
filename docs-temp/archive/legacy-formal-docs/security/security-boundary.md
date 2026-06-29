# 安全边界

> 文档状态：当前实现
> 适用范围：安全 / 部署 / 开发
> 最后更新：2026-06-26

## 1. 文档目的

本文档定义 OJOS 的 public、internal、worker-only 和 admin 边界，防止部署和开发中把内部能力暴露给普通用户。

## 2. 适用范围

适用于部署、API 开发、前端权限控制、Worker Link 和安全审计。

## 3. 当前实现

Gateway 是唯一公开 API 入口。Auth、Problem API、Judge API、PostgreSQL、Redis 和 artifact storage 是内部能力。Worker API 通过 Gateway 暴露，但必须使用 worker token。

## 4. 目标设计

生产环境应进一步通过 TLS、网络策略、secret manager、审计日志和最小权限容器加固安全边界。

## 5. 关键流程

用户请求进入 Gateway；Gateway 校验 JWT 并签名内部请求；内部服务验证 HMAC；worker 请求还要校验 `X-OJOS-Worker-Token` 和 task lease。

当前请求链路可以按四类边界理解：

| 边界 | 入口 | 认证方式 | 允许访问者 | 主要风险 |
| --- | --- | --- | --- | --- |
| public | Gateway `/api/*` | JWT 或匿名 public API | 浏览器、CLI、worker | 越权访问、路径泄露 |
| internal | `auth`、`problem-api`、`judge-api` 内部端口 | Gateway HMAC | Control Plane 内部服务 | 伪造转发头 |
| worker-only | `/api/judge/worker/*` | worker token + task lease | 已登记 worker | 旧 lease 覆盖结果 |
| admin | `/api/admin/*`、`/api/*/admin/*` | JWT + 后端权限 | 管理员 | 只靠前端隐藏按钮 |

这四类边界必须同时成立。举例说，worker token 通过只说明请求来自可识别 worker，并不代表它可以上传任意 task 的结果；admin 菜单隐藏只改善体验，并不等于后端授权。

## 6. 配置说明

关键配置包括 JWT secret、内部 HMAC key、worker token、PostgreSQL DSN、Redis 地址和 artifact root。生产 secret 不能写入 Git。

配置落点应按服务边界拆分。Gateway 需要 JWT 校验配置、内部签名 key、下游服务地址和公开监听端口；内部服务需要验证 Gateway 签名的 key、数据库或 Redis 连接信息；worker node 只需要 Gateway 地址、worker token、并发数、语言能力和本机 work dir。worker node 不需要 PostgreSQL DSN，也不需要 Redis 凭据。

部署文件应保持“示例可读、生产不可直接套用”的原则：`.env.example` 只能给出变量名和结构，不给出可用于生产的默认 secret；compose 文件可以暴露 Gateway 给宿主机，但不能把数据库、Redis、`problem-api` 或 `judge-api` 作为普通用户可直接访问的 host port。

## 7. 禁止事项

- 不公开 PostgreSQL/Redis。
- 不公开 `problem-api`/`judge-api` host port。
- worker 不直连 DB/Redis。
- worker 不挂载 Control Plane storage。
- 不使用危险默认容器权限。

## 8. 验收方式

普通用户访问 admin API 返回 403；伪造 `X-Auth-Verified` 失败；错误 worker token 不能注册；Public API 不返回内部路径。

最小静态验收命令：

```powershell
powershell -NoProfile -File scripts\verify-static.ps1 -SkipDockerBuild
```

运行验收需要按角色执行：

1. 未登录用户访问 `/dashboard` 应跳转登录页。
2. 普通用户直接请求 `/api/admin/health` 应返回 403。
3. 带错误 `X-OJOS-Worker-Token` 请求 `/api/judge/worker/register` 应返回 401。
4. worker 使用旧 `lease_version` 上传结果应返回冲突错误。
5. 题目详情、提交详情和 case 结果中不得出现服务端绝对路径。

如果某一步失败，应先判断失败发生在哪个边界：Gateway 未拦截、内部服务未验证 HMAC、worker lease 校验缺失，还是权限系统未生效。不要通过前端隐藏按钮来掩盖后端授权问题。

## 9. 常见问题

- 内部服务能被浏览器访问：部署边界错误。
- 前端隐藏按钮但 API 仍成功：后端权限缺失。
- worker 需要 DB 密码：部署方式错误。

## 10. 相关文档

- [内部 HMAC](internal-hmac.md)
- [Worker Token](worker-token.md)
- [路径泄露防护](path-leak-prevention.md)
