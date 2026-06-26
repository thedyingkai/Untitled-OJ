# Judge API

> 文档状态：当前实现
> 适用范围：开发 / 前端接入 / Judge Core
> 最后更新：2026-06-26

## 1. 文档目的

本文档说明提交、提交列表、提交详情、case 结果和语言列表 API，确保提交生命周期和权限规则明确。

## 2. 适用范围

适用于维护 `services/judge-api`、提交页面、提交列表、提交详情、状态轮询和语言配置的开发者。

## 3. 当前实现

基础路径为 `/api/judge`。用户使用 JWT 访问提交 API；普通用户默认只能查看自己的提交；管理员按权限查看更大范围。

主要端点：

| 方法 | 路径 | 认证 | 说明 |
| --- | --- | --- | --- |
| `POST` | `/api/judge/submissions` | JWT | 创建提交 |
| `GET` | `/api/judge/submissions` | JWT | 提交列表 |
| `GET` | `/api/judge/submissions/:id` | JWT + 所属权 | 提交详情 |
| `GET` | `/api/judge/submissions/:id/cases` | JWT + 所属权 | case 摘要 |
| `GET` | `/api/judge/submissions/:id/debug-logs` | admin/owner 调试权限 | 截断调试文本 |
| `GET` | `/api/judge/languages` | JWT | 可提交语言 |
| `POST` | `/api/judge/submissions/:id/cancel` | owner/admin | 取消提交 |

## 4. 目标设计

后续可扩展 contest queue、优先级和更多语言，但 task ownership 仍应由 PostgreSQL lease 管理。

## 5. 关键流程

用户提交代码后，Judge API 创建 submission 和 task，写入 PostgreSQL，并向 Redis signal stream 写入信号。worker claim 后评测并上传 result。前端详情页轮询直到终态。

## 6. 配置说明

语言列表来自后端配置和 worker 支持能力。代码大小、输出限制和 task lease TTL 来自 Judge API/worker 配置。

Redis Streams 在当前架构中是 signal history，不是任务所有权事实源。任务状态、attempt、lease 和终态结果以 PostgreSQL 为准。前端轮询时应停止于终态：`ACCEPTED`、`WRONG_ANSWER`、`COMPILE_ERROR`、`RUNTIME_ERROR`、`TIME_LIMIT_EXCEEDED`、`MEMORY_LIMIT_EXCEEDED`、`OUTPUT_LIMIT_EXCEEDED`、`SYSTEM_ERROR`、`CANCELLED`、`UNSUPPORTED_LANGUAGE`。

## 7. 安全边界

普通用户不能通过 `user_id` 查询他人提交。case API 不返回 stdout/stderr/checker log 路径，管理员调试也只能返回截断文本。

## 8. API 示例

```http
POST /api/judge/submissions
Content-Type: application/json
Authorization: Bearer <token>

{"problem_id":1,"language":"cpp17","code":"int main(){return 0;}"}
```

```http
GET /api/judge/submissions/:id
GET /api/judge/submissions/:id/cases
GET /api/judge/languages
```

错误语义：

| 状态码 | 场景 | 处理方式 |
| --- | --- | --- |
| 400 | language/code/problem_id 非法 | 显示表单错误 |
| 403 | 查看他人提交或无权提交 private 题 | 显示 403 |
| 404 | 题目或提交不存在 | 显示 404 |
| 409 | 提交已终态、旧 lease、重复结果 | 显示状态冲突 |
| 500 | worker/result storage 异常 | 管理员查看 debug log |

## 9. 常见问题

- 提交后一直 PENDING：检查 worker 是否在线。
- 一直 JUDGING：检查 lease heartbeat 和 stale recovery。
- UNSUPPORTED_LANGUAGE：检查 `GET /api/judge/languages`。

## 10. 相关文档

- [Judge 状态模型](../judge/judge-status-model.md)
- [Worker API](worker-api.md)
