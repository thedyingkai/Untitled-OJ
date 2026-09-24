# 控制面与 Auth 的权限边界

远程 Web 的 OIDC 身份、OJ 用户 JWT、控制面管理凭据和 workload JWT 分属不同信任边界，不能互相替代。

| 凭据 | 使用位置 | 授权范围 |
| --- | --- | --- |
| OIDC access token / HttpOnly Web session | Orchestrator `/api/v1` | 控制面的 viewer/operator/admin RBAC；Web mutation 还要求 CSRF 与既有幂等约束 |
| Auth 签发的用户 JWT | Gateway → Auth 与业务 API | 已认证 OJ 用户及其角色、权限 |
| `ORCHESTRATOR_AUTH_ADMIN_TOKEN` | 控制面 → Auth | Topology 管理接口和用户有效权限的只读查询；不作为用户 JWT 或 workload 凭据 |
| `ORCHESTRATOR_AUTH_WORKLOAD_TOKEN` | 控制面 → Auth workload issuer | 兑换绑定 Deployment/Node/generation 的短期 workload JWT |
| workload JWT | 已部署服务 → Gateway/API Binding | 受投影、Binding、revision 和 credential generation 约束的服务调用 |

## Web 功能权限查询

Web 以已认证的 HttpOnly session 调用 `POST /api/v1/auth/permissions:check`。控制面从服务端 principal 取用户标识，向 Auth 请求：

`GET /auth/admin/users/{user_id}/effective-permissions?scope_type=system`

这里的 `user_id` 必须是能够精确映射到 Auth 用户的正整数。任意 OIDC subject、浏览器提供的 caller header 或请求体中的身份都不能自行建立这种映射。当前没有新增第三方身份关联数据库；部署 IdP 时必须明确它与 Auth 用户 ID 的对应关系。

Auth 的该路由使用专用 `PermissionReadMiddleware`。匹配配置的管理凭据时，它只建立“允许读取有效权限”的请求上下文标记，不生成管理员 claims。此标记仅由只读 `UserEffective` 用例消费；其他管理员用例仍执行原有授权。普通已登录管理员走原有认证与授权路径。

该中间件不注册到用户列表、角色/权限修改、用户登录或 workload permission-check 路由。源码契约 `auth.api` 和 GoZero 注册入口保持同一分组，避免重新生成时丢失这条边界。

控制面的 HTTP 适配器严格核对 Auth 返回的用户 ID、system scope、响应状态和权限键格式。无法连接、身份无法映射、错误响应或格式不匹配都返回“不允许”，不使用 OIDC 的 admin 角色猜测 OJ 业务权限。

## 集成发现与修正

J 批真实服务验收发现两处接口衔接错误：控制面查询 URL 缺少 `effective-permissions` 后缀；Auth 原有通用 JWT 中间件无法接入独立的控制面管理凭据。修正保持现有 URL 和响应格式，新增上述限定于只读用途的认证适配，没有放宽普通用户、权限修改或 workload 调用的授权。

本机隔离环境实际验证了 PostgreSQL 持久化、Gateway → Auth 管理员初始化/登录，以及 HTTPS OIDC Code + PKCE → Web session → Auth 有效权限查询；拒绝路径覆盖无效令牌、错误 issuer/audience/签名、过期令牌、低权限 mutation 与缺失 CSRF。它不等于外部 IdP 生产部署或浏览器页面渲染验收。实现与生命周期入口见 [Go 服务生命周期](service-lifecycle.md)。
