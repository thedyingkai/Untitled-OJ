# Problem API

> 文档状态：当前实现
> 适用范围：开发 / 前端接入 / 题目管理
> 最后更新：2026-06-26

## 1. 文档目的

本文档说明题目浏览、题目 CRUD 和题目包校验 API，确保题目数据、权限和路径防泄露一致。

## 2. 适用范围

适用于维护 `services/problem-api`、题目列表/详情/编辑页面、题目包管理页面和题目包 validator 的开发者。

## 3. 当前实现

基础路径为 `/api/problem`。普通用户只能访问 public 或授权题目；创建、编辑、删除和题目包管理需要 owner 或管理员权限。

主要端点：

| 方法 | 路径 | 认证 | 说明 |
| --- | --- | --- | --- |
| `GET` | `/api/problem/problems` | JWT/可按策略 public | 分页、搜索、过滤题目 |
| `GET` | `/api/problem/problems/:id` | JWT/可按 visibility public | 题目详情 |
| `POST` | `/api/problem/problems` | problem create 权限 | 创建题目 |
| `PUT` | `/api/problem/problems/:id` | owner/admin | 更新题目 |
| `DELETE` | `/api/problem/problems/:id` | owner/admin | 删除题目 |
| `POST` | `/api/problem/problems/:id/package/validate` | owner/admin | 校验题目包 |
| `GET` | `/api/problem/problems/:id/package/cases` | owner/admin | 查看 case 摘要 |

## 4. 目标设计

题目包导入导出、对象存储和更完整的分组/子任务可后续扩展，但不允许引入 legacy 题目包格式。

## 5. 关键流程

题目创建后写入 PostgreSQL 元信息；题目包存放在 Control Plane storage；validator 读取 `problem.yaml` 和 `tests/cases.yaml`，返回错误/警告摘要。

## 6. 配置说明

题目包 storage root 来自服务配置。Public API 只返回题面、样例、限制、标签、visibility 和校验摘要。

列表 API 应支持分页参数，避免一次返回所有题目。`visibility`、keyword、difficulty、tags 等过滤条件必须由后端校验，不允许前端自行过滤 private 数据。题目包校验只返回相对逻辑信息和错误摘要，不返回 Control Plane 本地目录。

## 7. 安全边界

API 不返回服务器绝对路径。普通用户不能查看 private 且未授权题目。删除已有提交的题目不能破坏历史。

## 8. API 示例

```http
GET /api/problem/problems?page=1&page_size=20&keyword=sum
Authorization: Bearer <token>
```

```http
POST /api/problem/problems
PUT /api/problem/problems/:id
DELETE /api/problem/problems/:id
```

```http
POST /api/problem/problems/:id/package/validate
```

错误语义：

| 状态码 | 场景 | 处理方式 |
| --- | --- | --- |
| 400 | title/slug/limit 字段非法 | 前端显示表单错误 |
| 403 | 无权查看或编辑 | 显示 403 或隐藏管理入口 |
| 404 | 题目不存在或不可见 | 显示 404 |
| 409 | slug 冲突或状态冲突 | 提示冲突字段 |
| 500 | storage 或数据库异常 | 显示 request id |

## 9. 常见问题

- 题目不可见：检查 visibility 和 resource binding。
- slug 冲突：返回 409，前端显示字段错误。
- validator 报路径逃逸：检查 YAML 中是否包含绝对路径或 `..`。

## 10. 相关文档

- [Storage 与 Artifact 模型](../architecture/storage-artifact-model.md)
- [路径泄露防护](../security/path-leak-prevention.md)
