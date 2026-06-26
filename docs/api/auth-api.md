# Auth API

> 文档状态：当前实现
> 适用范围：开发 / 前端接入 / 安全
> 最后更新：2026-06-26

## 1. 文档目的

本文档说明认证、注册和当前用户 API，确保前端登录态、后端 JWT、roles 和 permissions 使用一致。

## 2. 适用范围

适用于维护 `services/auth`、前端 auth store、router guard 和权限调试页面的开发者。

## 3. 当前实现

基础路径为 `/api/auth`，通过 Gateway 访问。登录和注册是 public API，当前用户和 admin API 需要 JWT。

主要端点：

| 方法 | 路径 | 认证 | 说明 |
| --- | --- | --- | --- |
| `POST` | `/api/auth/register` | public | 创建普通用户 |
| `POST` | `/api/auth/login` | public | 返回 JWT 和用户摘要 |
| `GET` | `/api/auth/me` | JWT | 返回当前用户 roles/permissions |
| `GET` | `/api/auth/admin/users` | admin | 用户列表 |
| `GET` | `/api/auth/admin/roles` | admin | 角色列表 |
| `GET` | `/api/auth/admin/permissions` | admin | 权限点列表 |

## 4. 目标设计

后续可加入 refresh token、密码策略和账号状态，但不能返回密码哈希或 secret。

## 5. 关键流程

用户登录后前端保存 token，API client 自动附带 `Authorization: Bearer <token>`。刷新页面时调用 `/api/auth/me` 恢复用户信息。401 时清理登录态。

## 6. 配置说明

JWT secret 由部署环境提供。前端 API base URL 由 `VITE_API_BASE_URL` 提供。

前端登录态存储 token，但不存储密码。router guard 在页面刷新后调用 `/api/auth/me` 恢复用户信息。后端 token 失效时，前端必须清理登录态并跳转 `/login`。

## 7. 安全边界

登录失败不能泄露用户是否存在的敏感细节。`/api/auth/me` 只返回用户 id、username、roles、permissions，不返回密码字段。

## 8. API 示例

```http
POST /api/auth/login
Content-Type: application/json

{"username":"alice","password":"<password>"}
```

```json
{"token":"<jwt>","user_id":1,"username":"alice","roles":["user"],"permissions":[]}
```

```http
GET /api/auth/me
Authorization: Bearer <token>
```

错误语义：

| 状态码 | 场景 | 前端处理 |
| --- | --- | --- |
| 400 | 用户名或密码格式错误 | 显示字段错误 |
| 401 | token 缺失或失效 | 清理登录态 |
| 403 | 访问 admin API 但权限不足 | 显示 403 页面 |
| 409 | 注册用户名冲突 | 提示用户名已存在 |
| 500 | 服务异常 | 显示 request id 并提示稍后重试 |

## 9. 常见问题

- 登录后刷新丢失：检查 token 持久化和 `/api/auth/me`。
- admin 页面 403：检查 roles/permissions 是否来自后端。
- 401 循环：检查 token 过期处理。

## 10. 相关文档

- [权限模型](../architecture/permission-model.md)
- [权限管理安全](../security/permission-admin.md)
