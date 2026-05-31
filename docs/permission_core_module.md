# OJOS Permission Core 模块开发文档

## 一、模块定位

`Permission Core` 是 OJOS 的完整资源级权限核心。

它负责解决：

```text
谁可以在什么资源范围内执行什么操作
```

统一抽象为：

```text
Can(principal, permission, scope)
```

例如：

```text
Can(user:1, "judge.submit", system:0)
Can(user:2, "problem.edit", problem:7)
Can(user:3, "contest.manage", contest:5)
Can(user:4, "balloon.manage", contest:5)
Can(user:5, "module.install", system:0)
```

Permission Core 不负责登录、不负责 JWT、不负责 HTTP 鉴权。

当前边界是：

```text
Auth      负责用户、角色、登录、JWT 签发
Gateway   负责 JWT 验证和用户上下文透传
Permission Core 负责资源级权限判断
业务服务   负责选择具体要检查的权限点
```

---

## 二、当前完成状态

当前 Permission Core 已经完成：

```text
完整资源级权限数据库模型
resource_types
permissions
role_permissions
role_bindings
permission_assignments
resource_edges
permission_audit_logs

保留并兼容已有 users / roles / user_roles

shared/security/permission 权限检查器
HasUserPermission
RequireUserPermission
BindRole
AssignPermission
AddResourceEdge
RegisterResourceType
RegisterPermission
GrantRolePermission

judge-api 接入 judge.submit
普通 user 角色允许提交
直接 deny 可以覆盖普通角色权限
删除 deny 后权限恢复
```

当前状态可以记为：

```text
Permission Core v1 基础落地完成
```

---

## 三、核心设计目标

Permission Core 的设计目标是：

```text
未来新增模块不需要修改权限核心表结构
未来新增资源类型只需要注册 resource_type
未来新增权限点只需要注册 permission
未来新增角色只需要注册 role 和 role_permissions
未来新增业务关系只需要写入 resource_edges
未来新增授权只需要写入 role_bindings 或 permission_assignments
```

该模型用于支持：

```text
题库
比赛
训练
团队
组织
提交
榜单
气球
打印
帖子
答疑
模块启动器
未来新模块
```

---

## 四、核心概念

### 4.1 Principal

`Principal` 表示权限主体。

当前主要使用：

```text
user:{id}
```

但模型支持未来扩展：

```text
team:{id}
group:{id}
service:{id}
```

结构：

```text
principal_type
principal_id
```

示例：

```text
user:1
team:5
group:2
service:1
```

---

### 4.2 Scope

`Scope` 表示权限作用域。

结构：

```text
scope_type
scope_id
```

示例：

```text
system:0
problem:7
contest:3
group:2
team:5
submission:100
module:0
```

约定：

```text
system:0 表示全局作用域
problem:0 表示所有题目
contest:0 表示所有比赛
scope_id = 0 表示该类型资源的全局范围
```

---

### 4.3 Permission

`Permission` 是权限点。

示例：

```text
system.admin
judge.submit
problem.create
problem.edit
problem.manage.data
contest.manage
scoreboard.freeze
balloon.manage
print.operate
module.install
```

权限点命名规范：

```text
<domain>.<action>
<domain>.<subdomain>.<action>
```

示例：

```text
problem.create
problem.manage.data
contest.manage.problem
scoreboard.view.admin
```

---

### 4.4 Role

`Role` 是权限集合模板。

角色本身不包含作用域。

例如：

```text
contest_manager
```

表示这个角色拥有一组比赛管理能力。

但用户在哪个比赛上拥有该角色，需要通过：

```text
role_bindings
```

表达。

例如：

```text
user:3 -> contest_manager @ contest:5
```

---

## 五、数据库结构

Permission Core 保留已有：

```text
users
roles
user_roles
```

其中：

```text
user_roles = 用户的系统级全局角色
```

新增核心表：

```text
resource_types
permissions
role_permissions
role_bindings
permission_assignments
resource_edges
permission_audit_logs
```

---

## 六、resource_types

`resource_types` 是资源类型注册表。

用途：

```text
声明系统中有哪些资源类型
支持模块注册自己的资源类型
```

典型数据：

```text
system
module
problem
contest
group
team
submission
post
clarification
balloon
print
```

未来新增模块时，例如：

```text
training
homework
course
virtual_contest
```

只需要插入新的 resource_type，不需要改表。

字段：

```text
code
module_code
name
description
created_at
```

---

## 七、permissions

`permissions` 是权限点注册表。

用途：

```text
声明系统中有哪些权限点
支持模块注册自己的权限点
```

字段：

```text
code
module_code
name
description
created_at
```

典型权限点：

```text
system.admin

module.install
module.enable
module.disable
module.configure

problem.create
problem.view
problem.view.private
problem.edit
problem.delete
problem.manage.data
problem.manage.asset

judge.submit

submission.view.own
submission.view.all
submission.rejudge
submission.delete

contest.create
contest.view
contest.manage
contest.manage.participant
contest.manage.problem
contest.freeze
contest.roll
contest.publish

scoreboard.view
scoreboard.view.admin
scoreboard.freeze
scoreboard.roll
scoreboard.export

balloon.manage
balloon.deliver

print.request
print.manage
print.operate

forum.post
forum.moderate

clarification.ask
clarification.answer
clarification.publish
```

---

## 八、roles

`roles` 是角色表。

系统保留已有 `roles`，并扩展通用字段：

```text
module_code
description
is_system
created_at
```

当前内置角色：

```text
super_admin
admin
user

module_manager

problem_owner
problem_setter
problem_viewer
problem_data_manager

contest_owner
contest_manager
contest_judge
contest_participant

balloon_volunteer
print_operator
forum_moderator
```

角色分两类：

```text
系统级角色
资源级角色
```

系统级角色示例：

```text
super_admin
admin
user
module_manager
```

资源级角色示例：

```text
problem_owner
problem_setter
contest_manager
contest_participant
balloon_volunteer
print_operator
```

---

## 九、role_permissions

`role_permissions` 表示某个角色拥有哪些权限点。

字段：

```text
role_id
permission_code
created_at
```

主键：

```text
(role_id, permission_code)
```

注意：

```text
role_permissions 不带 scope
```

原因是：

```text
角色只定义能力模板
作用域由 role_bindings 决定
```

例如：

```text
contest_manager 拥有 contest.manage
contest_manager 拥有 contest.freeze
contest_manager 拥有 scoreboard.view.admin
```

但用户是否是某个比赛的 contest_manager，由 `role_bindings` 决定。

---

## 十、role_bindings

`role_bindings` 是资源级角色绑定表。

用途：

```text
声明某个权限主体在某个资源范围内拥有某个角色
```

字段：

```text
id

principal_type
principal_id

role_id

scope_type
scope_id

granted_by_type
granted_by_id

expires_at
created_at
```

唯一约束：

```text
(principal_type, principal_id, role_id, scope_type, scope_id)
```

示例：

```text
user:2 -> problem_setter @ problem:7
user:3 -> contest_manager @ contest:5
user:4 -> balloon_volunteer @ contest:5
team:9 -> contest_participant @ contest:5
```

---

## 十一、permission_assignments

`permission_assignments` 是直接授权 / 直接拒绝表。

用途：

```text
处理例外权限
临时授权
临时禁止
用户封禁
特殊操作
```

字段：

```text
id

principal_type
principal_id

permission_code

scope_type
scope_id

effect

granted_by_type
granted_by_id

reason
expires_at
created_at
```

`effect` 只能是：

```text
allow
deny
```

唯一约束：

```text
(principal_type, principal_id, permission_code, scope_type, scope_id)
```

示例：

```text
allow user:5 problem.edit @ problem:9
deny  user:6 contest.view @ contest:3
deny  user:7 judge.submit @ system:0
```

权限判断中：

```text
deny 优先于普通 allow 和角色权限
```

但：

```text
super_admin 不受 deny 影响
```

如果要撤销 super_admin，只能移除 super_admin 角色。

---

## 十二、resource_edges

`resource_edges` 用于表达资源继承关系。

用途：

```text
支持资源级权限继承
```

字段：

```text
id

parent_type
parent_id

child_type
child_id

relation
created_at
```

唯一约束：

```text
(parent_type, parent_id, child_type, child_id, relation)
```

示例：

```text
group:1   -> contest:3
contest:3 -> problem:7
contest:3 -> submission:100
contest:3 -> balloon:12
contest:3 -> print:20
```

这样可以支持：

```text
用户是 contest:3 的 contest_manager
因此可以管理 contest:3 下的 submission / balloon / print 等资源
```

---

## 十三、permission_audit_logs

`permission_audit_logs` 是权限审计日志表。

用途：

```text
记录权限变更历史
记录角色绑定历史
记录直接授权或拒绝历史
支持后台追踪和问题排查
```

字段：

```text
id

actor_type
actor_id

action

target_type
target_id

permission_code
role_id
role_name

scope_type
scope_id

effect
metadata

created_at
```

典型 action：

```text
role.bind
permission.assign
role.revoke
permission.revoke
resource.edge.add
resource.edge.remove
```

---

## 十四、权限判断规则

统一检查函数：

```text
HasPermission(principal, permission, scope)
```

用户场景中使用：

```text
HasUserPermission(user_id, permission, scope)
```

判断顺序如下。

---

### 14.1 super_admin 最高优先级

如果用户拥有：

```text
super_admin @ system:0
```

则直接允许所有权限。

该判断优先于 deny。

如果要撤销超级权限，需要移除用户的 super_admin 角色或绑定。

---

### 14.2 收集作用域链

例如检查：

```text
problem.edit @ problem:7
```

系统会收集候选作用域：

```text
problem:7
problem:0
contest:3
contest:0
group:1
group:0
system:0
```

其中：

```text
contest:3
group:1
```

来自 `resource_edges` 向上递归查询。

`type:0` 是类型级通配作用域。

`system:0` 是全局作用域。

---

### 14.3 检查直接 deny

检查：

```text
permission_assignments.effect = deny
```

如果命中，直接拒绝。

---

### 14.4 检查直接 allow

检查：

```text
permission_assignments.effect = allow
```

如果命中，允许。

---

### 14.5 检查全局 user_roles

`user_roles` 是系统级全局角色。

如果用户通过 `user_roles` 拥有某个角色，并且该角色通过 `role_permissions` 拥有目标权限，则允许。

例如：

```text
user -> judge.submit
```

---

### 14.6 检查资源级 role_bindings

检查用户是否在候选作用域上拥有某个角色：

```text
role_bindings.scope in collected_scopes
```

再检查该角色是否拥有目标权限：

```text
role_permissions.permission_code = target permission
```

命中则允许。

---

### 14.7 默认拒绝

如果没有任何规则命中，则拒绝。

---

## 十五、shared/security/permission

路径：

```text
services/shared/security/permission
```

该包提供统一权限检查能力，避免每个服务重复写 SQL。

核心类型：

```go
type Principal struct {
    Type string
    ID   int64
}

type Scope struct {
    Type string
    ID   int64
}
```

常量：

```go
const (
    PrincipalUser    = "user"
    PrincipalTeam    = "team"
    PrincipalGroup   = "group"
    PrincipalService = "service"

    ScopeSystem = "system"

    EffectAllow = "allow"
    EffectDeny  = "deny"

    RoleSuperAdmin = "super_admin"
)
```

---

## 十六、权限检查 API

### 16.1 HasUserPermission

```go
func HasUserPermission(
    ctx context.Context,
    db *pgxpool.Pool,
    userID int64,
    permissionCode string,
    scope Scope,
) (bool, error)
```

用途：

```text
检查某个用户是否拥有指定权限
```

示例：

```go
ok, err := permission.HasUserPermission(
    ctx,
    db,
    userID,
    "problem.edit",
    permission.Scope{Type: "problem", ID: problemID},
)
```

---

### 16.2 RequireUserPermission

```go
func RequireUserPermission(
    ctx context.Context,
    db *pgxpool.Pool,
    userID int64,
    permissionCode string,
    scope Scope,
) error
```

用途：

```text
权限不足时直接返回 permission.ErrForbidden
```

示例：

```go
if err := permission.RequireUserPermission(
    ctx,
    db,
    userID,
    "judge.submit",
    permission.SystemScope(),
); err != nil {
    return nil, err
}
```

---

### 16.3 HasPermission

```go
func HasPermission(
    ctx context.Context,
    db *pgxpool.Pool,
    principal Principal,
    permissionCode string,
    scope Scope,
) (bool, error)
```

用途：

```text
支持非用户主体，例如 team / group / service
```

当前主要使用用户主体。

---

## 十七、权限管理 API

### 17.1 BindRole

```go
func BindRole(
    ctx context.Context,
    db *pgxpool.Pool,
    actor Principal,
    target Principal,
    roleName string,
    scope Scope,
    expiresAt *time.Time,
) error
```

用途：

```text
给某个主体在某个资源作用域上绑定角色
```

示例：

```go
permission.BindRole(
    ctx,
    db,
    permission.UserPrincipal(adminID),
    permission.UserPrincipal(userID),
    "problem_setter",
    permission.Scope{Type: "problem", ID: problemID},
    nil,
)
```

表示：

```text
user:{userID} 是 problem:{problemID} 的 problem_setter
```

---

### 17.2 AssignPermission

```go
func AssignPermission(
    ctx context.Context,
    db *pgxpool.Pool,
    actor Principal,
    target Principal,
    permissionCode string,
    scope Scope,
    effect string,
    reason string,
    expiresAt *time.Time,
) error
```

用途：

```text
直接允许或拒绝某个主体在某个作用域上的某个权限
```

示例：

```go
permission.AssignPermission(
    ctx,
    db,
    permission.UserPrincipal(adminID),
    permission.UserPrincipal(userID),
    "judge.submit",
    permission.SystemScope(),
    permission.EffectDeny,
    "temporary banned",
    nil,
)
```

---

### 17.3 AddResourceEdge

```go
func AddResourceEdge(
    ctx context.Context,
    db *pgxpool.Pool,
    parent Scope,
    child Scope,
    relation string,
) error
```

用途：

```text
建立资源继承关系
```

示例：

```go
permission.AddResourceEdge(
    ctx,
    db,
    permission.Scope{Type: "contest", ID: contestID},
    permission.Scope{Type: "problem", ID: problemID},
    "contains",
)
```

表示：

```text
contest:{contestID} 包含 problem:{problemID}
```

---

### 17.4 RegisterResourceType

```go
func RegisterResourceType(
    ctx context.Context,
    db *pgxpool.Pool,
    code string,
    moduleCode string,
    name string,
    description string,
) error
```

用途：

```text
模块注册自己的资源类型
```

---

### 17.5 RegisterPermission

```go
func RegisterPermission(
    ctx context.Context,
    db *pgxpool.Pool,
    code string,
    moduleCode string,
    name string,
    description string,
) error
```

用途：

```text
模块注册自己的权限点
```

---

### 17.6 GrantRolePermission

```go
func GrantRolePermission(
    ctx context.Context,
    db *pgxpool.Pool,
    roleName string,
    permissionCode string,
) error
```

用途：

```text
给角色授予权限点
```

---

## 十八、业务服务如何接入

### 18.1 Gateway

Gateway 只负责：

```text
JWT 验证
用户上下文透传
```

Gateway 不做具体权限判断。

原因：

```text
Gateway 不理解具体业务语义
```

例如：

```text
POST /api/problems/:id/testcases
```

到底需要：

```text
problem.edit
```

还是：

```text
problem.manage.data
```

应该由 `problem-api` 决定，而不是 Gateway 决定。

---

### 18.2 judge-api

当前已接入：

```text
POST /judge/submissions -> judge.submit @ system:0
```

逻辑：

```text
从 authctx 读取 user_id
调用 RequireUserPermission
权限通过后创建 submission
发布 submission.created
```

---

### 18.3 problem-api

后续应接入：

```text
POST /problems
    -> problem.create @ system:0

GET /problems/:id
    -> problem.view @ problem:{id}

POST /problems/:id/testcases
    -> problem.manage.data @ problem:{id}

POST /problems/:id/assets
    -> problem.manage.asset @ problem:{id}
```

题目创建成功后应自动绑定：

```text
creator -> problem_owner @ problem:{id}
```

---

### 18.4 contest-api

后续应接入：

```text
POST /contests
    -> contest.create @ system:0

POST /contests/:id/problems
    -> contest.manage.problem @ contest:{id}

POST /contests/:id/participants
    -> contest.manage.participant @ contest:{id}

POST /contests/:id/freeze
    -> contest.freeze @ contest:{id}

POST /contests/:id/roll
    -> contest.roll @ contest:{id}
```

比赛创建成功后应自动绑定：

```text
creator -> contest_owner @ contest:{id}
```

---

### 18.5 launcher

后续应接入：

```text
GET /launcher/modules
    -> launcher.view @ system:0

POST /launcher/install
    -> launcher.install @ system:0

POST /launcher/enable
    -> launcher.enable @ system:0

POST /launcher/disable
    -> launcher.disable @ system:0
```

---

## 十九、当前真实验收结果

当前已完成以下真实验证：

```text
1. 新建 permtest 用户
2. permtest 只有 user 角色
3. permtest 登录获得 JWT
4. permtest 可以正常提交代码
5. submission 13 写入 user_id = 2
6. submission 13 最终 ACCEPTED
7. 给 permtest 写入 judge.submit @ system:0 deny
8. 再次提交被 forbidden 拦截
9. 删除 deny
10. permtest 再次提交
11. submission 14 写入 user_id = 2
12. submission 14 最终 ACCEPTED
```

该验证说明：

```text
普通 user 角色通过 role_permissions 获得 judge.submit
permission_assignments.deny 可以覆盖普通 user 角色权限
删除 deny 后权限恢复
judge-api 已经真实接入 shared permission checker
```

---

## 二十、当前限制

当前 Permission Core 仍有一些工程层面待完善点：

```text
权限错误响应还未统一 JSON 包装
权限管理 API 尚未独立成 permission-api
权限管理 UI 尚未实现
resource_edges 尚未在具体 problem / contest 创建时自动写入
audit logs 已有表和写入函数，但缺少后台查询接口
role revoke / permission revoke API 尚未封装
```

其中最先需要补的是：

```text
统一错误响应
```

例如将：

```text
forbidden
```

包装为：

```json
{
  "code": 40301,
  "msg": "forbidden"
}
```

---

## 二十一、后续计划

建议后续顺序：

```text
1. 统一权限错误 JSON 响应
2. problem-api 接入 Permission Core
3. 创建 problem 后自动绑定 problem_owner
4. problem-api 创建测试点时检查 problem.manage.data
5. contest-api 接入 Permission Core
6. 创建 contest 后自动绑定 contest_owner
7. 建立 contest -> problem 的 resource_edges
8. permission-api / admin API
9. 权限管理前端
10. module-registry 使用 permission 注册资源类型和权限点
```

---

## 二十二、当前结论

Permission Core 当前已经完成从“简单角色”到“完整资源级权限”的基础升级。

当前权限核心模型支持：

```text
principal_type / principal_id
scope_type / scope_id
system:0
type:0
resource_edges 继承
allow / deny
super_admin
全局 user_roles
资源级 role_bindings
shared permission checker
业务服务接入
```

该模型后续可以支撑：

```text
题库
比赛
榜单
气球
打印
帖子
答疑
模块启动器
训练
作业
课程
未来新模块
```

并且无需因为新增业务模块修改权限核心表结构。
