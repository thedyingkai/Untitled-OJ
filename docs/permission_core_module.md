# OJOS Permission Core 模块开发文档

## 一、模块定位

`Permission Core` 是 OJOS 的完整资源级权限核心。

它负责解决的问题是：

```text
谁可以在什么资源范围内执行什么操作
```

统一抽象为：

```text
Can(principal, permission, scope)
```

也就是：

```text
Can(权限主体, 权限点, 资源作用域)
```

示例：

```text
Can(user:1, "judge.submit", system:0)
Can(user:2, "problem.edit", problem:7)
Can(user:3, "problem.manage.data", problem:7)
Can(user:4, "contest.manage", contest:5)
Can(user:5, "contest.freeze", contest:5)
Can(user:6, "scoreboard.roll", contest:5)
Can(user:7, "balloon.manage", contest:5)
Can(user:8, "print.operate", contest:5)
Can(user:9, "module.install", system:0)
```

Permission Core 的定位是平台内核能力，不属于某一个业务模块。

它不是 Auth。

它不是 Gateway。

它不是 judge-api。

它不是 problem-api。

它也不是 contest-api。

它的职责边界是：

```text
Auth
    负责用户、密码、登录、JWT、基础角色读取

Gateway
    负责 JWT 验证、入口鉴权、可信用户上下文透传

Permission Core
    负责资源级权限判断

业务服务
    负责选择具体要检查的权限点和资源作用域
```

例如：

```text
用户提交代码
    Gateway 负责确认用户已登录
    judge-api 负责检查 judge.submit @ system:0
    Permission Core 负责判断该用户是否拥有这个权限
```

又例如：

```text
用户编辑题目
    Gateway 负责确认用户已登录
    problem-api 负责检查 problem.edit @ problem:{id}
    Permission Core 负责判断该用户是否拥有这个权限
```

Permission Core 不应该处理：

```text
用户密码
JWT 签发
HTTP 路由
反向代理
题目 CRUD
比赛 CRUD
提交创建
判题执行
榜单计算
模块安装流程
```

它只处理权限判断和权限数据维护。

---

## 二、当前版本状态

当前 Permission Core 已经完成基础落地。

当前版本可以记为：

```text
Permission Core v1
```

当前已完成能力：

```text
完整资源级权限数据库模型
resource_types
permissions
role_permissions
role_bindings
permission_assignments
resource_edges
permission_audit_logs

兼容已有 users / roles / user_roles

shared/security/permission 权限检查器
HasPermission
HasUserPermission
RequireUserPermission
BindRole
AssignPermission
AddResourceEdge
RegisterResourceType
RegisterPermission
GrantRolePermission

支持 system:0
支持 type:0
支持资源继承
支持全局角色
支持资源级角色
支持直接 allow
支持直接 deny
支持 super_admin
支持过期时间 expires_at
支持权限审计日志

judge-api 已接入 judge.submit
普通 user 角色允许提交
permission_assignments.deny 可以覆盖普通 user 角色权限
删除 deny 后权限恢复
```

当前已完成真实验收：

```text
permtest 用户只有 user 角色
permtest 可以提交代码
submission 正确写入 user_id
提交最终 ACCEPTED
写入 judge.submit @ system:0 deny 后提交被 forbidden 拦截
删除 deny 后提交恢复
```

这说明：

```text
role_permissions 生效
user_roles 生效
permission_assignments.deny 生效
deny 删除后权限恢复
judge-api 已真实接入 Permission Core
```

当前尚未完成的管理能力：

```text
permission-api
权限管理前端
角色绑定管理接口
直接授权 / 拒绝管理接口
权限审计日志查询接口
统一 JSON 错误响应
resource_edges 自动维护
role revoke API
permission revoke API
```

这些不影响当前 Permission Core 的核心判断模型。

---

## 三、设计目标

Permission Core 的核心设计目标是：**未来新增模块不需要修改权限核心表结构**。

也就是说，后续即使新增：

```text
problem-api
contest-api
scoreboard-api
balloon-service
print-service
forum-service
clarification-service
module-registry
launcher
training-api
homework-api
course-api
virtual-contest-api
```

也不应该为了新增这些模块修改 Permission Core 的基础表结构。

新增模块时，只应该做：

```text
注册 resource_type
注册 permission
注册 role
注册 role_permissions
写入 role_bindings
写入 permission_assignments
写入 resource_edges
```

例如新增 `contest-core` 时，可以注册：

```text
resource_type:
    contest

permissions:
    contest.create
    contest.view
    contest.manage
    contest.manage.participant
    contest.manage.problem
    contest.freeze
    contest.roll
    contest.publish

roles:
    contest_owner
    contest_manager
    contest_participant
```

例如新增 `balloon-service` 时，可以注册：

```text
resource_type:
    balloon

permissions:
    balloon.manage
    balloon.deliver

roles:
    balloon_volunteer
```

例如新增 `launcher` 时，可以注册：

```text
resource_type:
    module

permissions:
    module.install
    module.enable
    module.disable
    module.configure
    launcher.view
    launcher.install
    launcher.uninstall
    launcher.enable
    launcher.disable
```

Permission Core 的设计原则：

```text
权限点是字符串，不写死 enum
资源类型是字符串，不写死 enum
角色是数据库数据，不写死 enum
授权关系通过表维护
资源继承通过 resource_edges 维护
直接例外通过 permission_assignments 维护
业务服务只调用统一检查函数
```

这样可以避免后续每次新增一个题型、赛制、运营能力都要改权限核心代码。

---

## 四、核心概念

Permission Core 中有四个最核心概念：

```text
Principal
Scope
Permission
Role
```

---

## 五、Principal 权限主体

`Principal` 表示权限主体，也就是“谁”。

结构：

```text
principal_type
principal_id
```

当前主要使用：

```text
user:{id}
```

例如：

```text
user:1
user:2
user:100
```

未来可以扩展：

```text
team:{id}
group:{id}
service:{id}
```

示例：

```text
team:5
group:2
service:1
```

推荐 Go 类型：

```go
type Principal struct {
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
)
```

辅助函数：

```go
func UserPrincipal(userID int64) Principal {
    return Principal{
        Type: PrincipalUser,
        ID:   userID,
    }
}
```

当前 Permission Core 主要检查用户主体：

```text
user:{id}
```

但保留 `team / group / service` 的原因是后续会出现：

```text
团队参赛
组织权限
服务间调用
机器人账号
模块服务账号
```

例如：

```text
team:9 -> contest_participant @ contest:5
group:1 -> problem_viewer @ problem:7
service:3 -> submission.rejudge @ system:0
```

---

## 六、Scope 资源作用域

`Scope` 表示权限作用域，也就是“在哪里”。

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
balloon:12
print:20
post:30
clarification:40
```

推荐 Go 类型：

```go
type Scope struct {
    Type string
    ID   int64
}
```

常量：

```go
const (
    ScopeSystem = "system"
)
```

辅助函数：

```go
func SystemScope() Scope {
    return Scope{
        Type: ScopeSystem,
        ID:   0,
    }
}
```

核心约定：

```text
system:0 表示全局系统作用域
problem:0 表示所有题目
contest:0 表示所有比赛
module:0 表示所有模块
scope_id = 0 表示该类型资源的全局范围
```

例如：

```text
problem.edit @ problem:7
```

表示只能编辑第 7 题。

```text
problem.edit @ problem:0
```

表示可以编辑所有题目。

```text
contest.manage @ contest:5
```

表示可以管理第 5 场比赛。

```text
contest.manage @ contest:0
```

表示可以管理所有比赛。

```text
system.admin @ system:0
```

表示系统全局管理员能力。

---

## 七、Permission 权限点

`Permission` 表示具体操作能力，也就是“能做什么”。

权限点使用字符串表示。

命名规范：

```text
<domain>.<action>
<domain>.<subdomain>.<action>
```

示例：

```text
judge.submit
problem.create
problem.edit
problem.manage.data
problem.manage.asset
contest.create
contest.manage
contest.manage.participant
contest.manage.problem
contest.freeze
scoreboard.roll
balloon.manage
print.operate
module.install
```

权限点不应该写成 Go enum。

原因：

```text
未来模块可以注册新权限点
权限点需要由数据库和模块 manifest 管理
不应该每新增一个模块就改核心代码
```

权限点应该写入：

```text
permissions
```

表中。

推荐权限点分类：

```text
system.*
module.*
launcher.*
problem.*
judge.*
submission.*
contest.*
scoreboard.*
balloon.*
print.*
forum.*
clarification.*
```

---

## 八、Role 角色

`Role` 表示权限集合模板。

角色本身不包含作用域。

例如：

```text
contest_manager
```

表示这个角色拥有一组比赛管理能力，例如：

```text
contest.manage
contest.manage.participant
contest.manage.problem
contest.freeze
scoreboard.view.admin
```

但是用户在哪个比赛上拥有 `contest_manager`，不由 `roles` 表决定，而由：

```text
role_bindings
```

决定。

例如：

```text
user:3 -> contest_manager @ contest:5
```

表示用户 3 是比赛 5 的比赛管理员。

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

系统级角色通常通过：

```text
user_roles
```

绑定。

资源级角色通常通过：

```text
role_bindings
```

绑定。

---

## 九、数据库结构总览

Permission Core 保留并兼容已有表：

```text
users
roles
user_roles
```

并新增核心表：

```text
resource_types
permissions
role_permissions
role_bindings
permission_assignments
resource_edges
permission_audit_logs
```

整体关系：

```text
users
  ↓
user_roles
  ↓
roles
  ↓
role_permissions
  ↓
permissions

principals
  ↓
role_bindings
  ↓
roles
  ↓
role_permissions
  ↓
permissions

principals
  ↓
permission_assignments
  ↓
permissions

resource_edges
  ↓
scope inheritance
```

其中：

```text
user_roles
```

表示用户的系统级全局角色。

```text
role_bindings
```

表示某个权限主体在某个资源作用域上拥有某个角色。

```text
permission_assignments
```

表示直接 allow 或 deny 某个权限。

```text
resource_edges
```

表示资源之间的继承关系。

---

## 十、resource_types 表

`resource_types` 是资源类型注册表。

用途：

```text
声明系统中有哪些资源类型
支持模块注册自己的资源类型
避免在代码中写死资源类型 enum
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

未来可能新增：

```text
training
homework
course
virtual_contest
dataset
checker
runner
language_pack
```

推荐字段：

```text
code
module_code
name
description
created_at
```

字段说明：

| 字段            | 含义                     |
| ------------- | ---------------------- |
| `code`        | 资源类型代码，例如 `problem`    |
| `module_code` | 所属模块，例如 `problem-core` |
| `name`        | 展示名称                   |
| `description` | 说明                     |
| `created_at`  | 创建时间                   |

示例：

```sql
INSERT INTO resource_types(code, module_code, name, description)
VALUES
    ('problem', 'problem-core', 'Problem', '题目资源'),
    ('contest', 'contest-core', 'Contest', '比赛资源'),
    ('module', 'module-registry', 'Module', '模块资源')
ON CONFLICT(code) DO NOTHING;
```

后续模块安装时，Launcher 可以根据 `ojos.module.yaml` 自动注册 resource_types。

---

## 十一、permissions 表

`permissions` 是权限点注册表。

用途：

```text
声明系统中有哪些权限点
支持模块注册自己的权限点
避免在代码中写死权限点 enum
```

推荐字段：

```text
code
module_code
name
description
created_at
```

字段说明：

| 字段            | 含义                      |
| ------------- | ----------------------- |
| `code`        | 权限点代码，例如 `judge.submit` |
| `module_code` | 所属模块，例如 `judge-core`    |
| `name`        | 展示名称                    |
| `description` | 说明                      |
| `created_at`  | 创建时间                    |

典型权限点：

```text
system.admin

module.install
module.enable
module.disable
module.configure

launcher.view
launcher.install
launcher.uninstall
launcher.enable
launcher.disable

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

示例：

```sql
INSERT INTO permissions(code, module_code, name, description)
VALUES
    ('judge.submit', 'judge-core', 'Submit Code', '提交代码'),
    ('problem.create', 'problem-core', 'Create Problem', '创建题目'),
    ('problem.edit', 'problem-core', 'Edit Problem', '编辑题目')
ON CONFLICT(code) DO NOTHING;
```

---

## 十二、roles 表

`roles` 是角色表。

系统保留已有 `roles`，并扩展通用字段：

```text
id
name
module_code
description
is_system
created_at
```

字段说明：

| 字段            | 含义       |
| ------------- | -------- |
| `id`          | 角色 ID    |
| `name`        | 角色名      |
| `module_code` | 所属模块     |
| `description` | 描述       |
| `is_system`   | 是否系统内置角色 |
| `created_at`  | 创建时间     |

当前内置角色建议包括：

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

角色命名规范：

```text
snake_case
```

示例：

```text
problem_owner
contest_manager
balloon_volunteer
```

不推荐：

```text
ProblemOwner
contest-manager
CONTEST_MANAGER
```

---

## 十三、role_permissions 表

`role_permissions` 表示某个角色拥有哪些权限点。

字段：

```text
role_id
permission_code
created_at
```

推荐主键或唯一约束：

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
作用域由 user_roles 或 role_bindings 决定
```

例如：

```text
contest_manager 拥有 contest.manage
contest_manager 拥有 contest.freeze
contest_manager 拥有 scoreboard.view.admin
```

但是用户在哪个比赛上是 contest_manager，由：

```text
role_bindings
```

决定。

示例：

```sql
INSERT INTO role_permissions(role_id, permission_code)
SELECT r.id, 'judge.submit'
FROM roles r
WHERE r.name = 'user'
ON CONFLICT DO NOTHING;
```

表示：

```text
user 角色拥有 judge.submit
```

这并不代表某个用户一定拥有该角色，还需要 `user_roles` 或 `role_bindings` 绑定。

---

## 十四、user_roles 表

`user_roles` 是已有表，用于维护用户与系统级角色的关系。

当前定义为：

```text
用户的系统级全局角色绑定
```

字段：

```text
user_id
role_id
```

示例：

```text
permtest -> user
admin -> user
admin -> super_admin
```

`user_roles` 中的角色是全局的。

例如：

```text
user:2 -> user
```

表示用户 2 拥有 `user` 这个系统级角色。

如果 `user` 角色通过 `role_permissions` 拥有：

```text
judge.submit
```

则用户 2 默认拥有：

```text
judge.submit @ system:0
```

当前注册用户默认绑定：

```text
user
```

这由 Auth 模块完成。

---

## 十五、role_bindings 表

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

推荐唯一约束：

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

解释：

```text
user:2 是 problem:7 的出题人
user:3 是 contest:5 的比赛管理员
user:4 是 contest:5 的气球志愿者
team:9 是 contest:5 的参赛队伍
```

SQL 示例：

```sql
INSERT INTO role_bindings(
    principal_type,
    principal_id,
    role_id,
    scope_type,
    scope_id,
    granted_by_type,
    granted_by_id
)
SELECT
    'user',
    2,
    r.id,
    'problem',
    7,
    'user',
    1
FROM roles r
WHERE r.name = 'problem_setter'
ON CONFLICT DO NOTHING;
```

---

## 十六、permission_assignments 表

`permission_assignments` 是直接授权 / 直接拒绝表。

用途：

```text
处理例外权限
临时授权
临时禁止
封禁用户
覆盖普通角色权限
特殊操作授权
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

推荐唯一约束：

```text
(principal_type, principal_id, permission_code, scope_type, scope_id)
```

示例：

```text
allow user:5 problem.edit @ problem:9
deny  user:6 contest.view @ contest:3
deny  user:7 judge.submit @ system:0
allow user:8 scoreboard.roll @ contest:5
```

当前真实验证中使用过：

```text
deny user:permtest judge.submit @ system:0
```

写入后，permtest 再提交代码会被拦截。

删除 deny 后，permtest 通过 `user` 角色重新获得 `judge.submit`，提交恢复正常。

---

### 16.1 deny 的优先级

当前规则：

```text
deny 优先于普通 allow 和角色权限
```

也就是说：

```text
用户通过 user 角色拥有 judge.submit
但 permission_assignments 中存在 judge.submit deny
则最终拒绝
```

但是：

```text
super_admin 高于 deny
```

如果用户拥有 `super_admin`，则直接允许，不检查 deny。

原因是：

```text
super_admin 是系统最高权限
如果要限制 super_admin，应该移除 super_admin 角色
不应该用 deny 去覆盖 super_admin
```

---

### 16.2 expires_at

`permission_assignments` 支持：

```text
expires_at
```

用于临时授权或临时拒绝。

例如：

```text
临时禁止用户提交 24 小时
临时允许用户管理某场比赛
临时授予验题权限
```

权限判断时应忽略已经过期的记录：

```sql
expires_at IS NULL OR expires_at > NOW()
```

---

## 十七、resource_edges 表

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

推荐唯一约束：

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

含义：

```text
group:1 包含 contest:3
contest:3 包含 problem:7
contest:3 包含 submission:100
contest:3 包含 balloon:12
contest:3 包含 print:20
```

这样可以支持：

```text
用户是 contest:3 的 contest_manager
因此可以管理 contest:3 下的 submission / balloon / print
```

资源继承查询时，应从当前 scope 向父级递归。

例如检查：

```text
submission.view.all @ submission:100
```

如果存在：

```text
contest:3 -> submission:100
```

则候选 scope 包括：

```text
submission:100
submission:0
contest:3
contest:0
system:0
```

如果用户拥有：

```text
contest_manager @ contest:3
```

且 `contest_manager` 拥有：

```text
submission.view.all
```

则允许。

---

## 十八、permission_audit_logs 表

`permission_audit_logs` 是权限审计日志表。

用途：

```text
记录权限变更历史
记录角色绑定历史
记录直接授权或拒绝历史
支持后台追踪
支持问题排查
支持安全审计
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
role.revoke
permission.assign
permission.revoke
resource.edge.add
resource.edge.remove
permission.register
resource_type.register
```

示例：

```text
user:1 给 user:2 绑定 problem_setter @ problem:7
user:1 给 user:3 deny judge.submit @ system:0
user:1 添加 contest:5 -> problem:7 资源关系
```

当前已有表和基础写入能力，但缺少：

```text
审计日志查询 API
权限后台 UI
筛选和分页
```

后续应由 `permission-api` 或后台管理模块实现。

---

## 十九、权限判断规则

统一检查函数：

```text
HasPermission(principal, permission, scope)
```

用户场景中使用：

```text
HasUserPermission(user_id, permission, scope)
```

业务服务常用：

```text
RequireUserPermission(user_id, permission, scope)
```

如果无权限，则返回：

```text
ErrForbidden
```

当前判断顺序如下。

---

### 19.1 super_admin 最高优先级

如果用户拥有：

```text
super_admin
```

则直接允许所有权限。

这个判断优先于 deny。

如果要撤销超级权限，应移除用户的 `super_admin` 角色，而不是写 deny。

例如：

```sql
DELETE FROM user_roles
WHERE user_id = 1
  AND role_id = (SELECT id FROM roles WHERE name = 'super_admin');
```

---

### 19.2 收集候选作用域

例如检查：

```text
problem.edit @ problem:7
```

Permission Core 会收集候选作用域：

```text
problem:7
problem:0
parent scopes...
system:0
```

如果 `resource_edges` 中存在：

```text
contest:3 -> problem:7
group:1 -> contest:3
```

则候选作用域可能包括：

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
type:0
```

表示某类资源的全局作用域。

例如：

```text
problem:0
contest:0
group:0
```

---

### 19.3 检查直接 deny

检查：

```text
permission_assignments.effect = deny
```

并且：

```text
principal 匹配
permission_code 匹配
scope 在候选作用域内
expires_at 未过期
```

如果命中，直接拒绝。

---

### 19.4 检查直接 allow

检查：

```text
permission_assignments.effect = allow
```

条件同上。

如果命中，允许。

---

### 19.5 检查全局 user_roles

检查用户是否通过 `user_roles` 拥有某个角色，并且该角色通过 `role_permissions` 拥有目标权限。

例如：

```text
user:2 -> user
user -> judge.submit
```

则：

```text
user:2 has judge.submit @ system:0
```

当前普通用户提交代码就是通过这个机制实现。

---

### 19.6 检查资源级 role_bindings

检查用户是否在候选作用域上拥有某个角色：

```text
role_bindings.scope_type / scope_id in candidate scopes
```

再检查该角色是否拥有目标权限：

```text
role_permissions.permission_code = target permission
```

命中则允许。

例如：

```text
user:2 -> problem_setter @ problem:7
problem_setter -> problem.edit
```

则：

```text
user:2 可以 problem.edit @ problem:7
```

---

### 19.7 默认拒绝

如果以上规则都没有命中，则拒绝。

默认拒绝是权限系统的基本原则。

也就是说，不能因为没有配置 deny 就允许。

只有明确 allow、角色授权或 super_admin 才允许。

---

## 二十、shared/security/permission API

路径：

```text
services/shared/security/permission
```

当前 Permission Core 以 Go 包形式提供给各业务服务使用。

---

### 20.1 Principal

推荐结构：

```go
type Principal struct {
    Type string
    ID   int64
}
```

辅助函数：

```go
func UserPrincipal(userID int64) Principal
```

示例：

```go
principal := permission.UserPrincipal(userID)
```

---

### 20.2 Scope

推荐结构：

```go
type Scope struct {
    Type string
    ID   int64
}
```

辅助函数：

```go
func SystemScope() Scope
```

示例：

```go
scope := permission.SystemScope()
```

题目作用域：

```go
scope := permission.Scope{
    Type: "problem",
    ID:   problemID,
}
```

比赛作用域：

```go
scope := permission.Scope{
    Type: "contest",
    ID:   contestID,
}
```

---

### 20.3 HasUserPermission

函数：

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
    permission.Scope{
        Type: "problem",
        ID:   problemID,
    },
)
if err != nil {
    return err
}
if !ok {
    return permission.ErrForbidden
}
```

---

### 20.4 RequireUserPermission

函数：

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
权限不足时直接返回 ErrForbidden
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

业务服务中推荐优先使用 `RequireUserPermission`。

---

### 20.5 HasPermission

函数：

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

未来 team-based contest 可以用：

```go
permission.HasPermission(
    ctx,
    db,
    permission.Principal{
        Type: "team",
        ID:   teamID,
    },
    "contest.participate",
    permission.Scope{
        Type: "contest",
        ID:   contestID,
    },
)
```

---

### 20.6 BindRole

函数：

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
err := permission.BindRole(
    ctx,
    db,
    permission.UserPrincipal(adminID),
    permission.UserPrincipal(userID),
    "problem_setter",
    permission.Scope{
        Type: "problem",
        ID:   problemID,
    },
    nil,
)
```

表示：

```text
user:{userID} 是 problem:{problemID} 的 problem_setter
```

---

### 20.7 AssignPermission

函数：

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
err := permission.AssignPermission(
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

表示：

```text
直接禁止 user:{userID} 在 system:0 上 judge.submit
```

---

### 20.8 AddResourceEdge

函数：

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
err := permission.AddResourceEdge(
    ctx,
    db,
    permission.Scope{
        Type: "contest",
        ID:   contestID,
    },
    permission.Scope{
        Type: "problem",
        ID:   problemID,
    },
    "contains",
)
```

表示：

```text
contest:{contestID} contains problem:{problemID}
```

---

### 20.9 RegisterResourceType

函数：

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

示例：

```go
permission.RegisterResourceType(
    ctx,
    db,
    "training",
    "training-core",
    "Training",
    "训练资源",
)
```

---

### 20.10 RegisterPermission

函数：

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

示例：

```go
permission.RegisterPermission(
    ctx,
    db,
    "training.manage",
    "training-core",
    "Manage Training",
    "管理训练",
)
```

---

### 20.11 GrantRolePermission

函数：

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

示例：

```go
permission.GrantRolePermission(
    ctx,
    db,
    "training_manager",
    "training.manage",
)
```

---

## 二十一、业务服务接入方式

### 21.1 Gateway

Gateway 不做资源级权限判断。

Gateway 只负责：

```text
JWT 验证
用户上下文透传
```

Gateway 不应该写：

```text
check problem.edit
check judge.submit
check contest.manage
```

这些由业务服务调用 Permission Core 完成。

---

### 21.2 Auth

Auth 不做资源级权限判断。

Auth 负责：

```text
注册
登录
JWT
基础角色
```

Auth 注册用户时绑定：

```text
user
```

资源级权限由 Permission Core 判断。

---

### 21.3 judge-api

当前已接入：

```text
POST /judge/submissions
    -> judge.submit @ system:0
```

逻辑：

```text
从 authctx 读取 user_id
调用 RequireUserPermission
权限通过后创建 submission
写入 Redis Stream
```

示例：

```go
user, ok := authctx.FromContext(l.ctx)
if !ok || user == nil || user.UserID <= 0 {
    return nil, errors.New("unauthorized")
}

if err := permission.RequireUserPermission(
    l.ctx,
    l.svcCtx.DB,
    user.UserID,
    "judge.submit",
    permission.SystemScope(),
); err != nil {
    return nil, err
}
```

---

### 21.4 problem-api

后续应接入：

```text
POST /problem/problems
    -> problem.create @ system:0

GET /problem/problems/:id
    -> problem.view @ problem:{id}

PUT /problem/problems/:id
    -> problem.edit @ problem:{id}

POST /problem/problems/:id/testcases
    -> problem.manage.data @ problem:{id}

POST /problem/problems/:id/assets
    -> problem.manage.asset @ problem:{id}
```

题目创建成功后应自动绑定：

```text
creator -> problem_owner @ problem:{id}
```

即：

```go
permission.BindRole(
    ctx,
    db,
    permission.UserPrincipal(creatorID),
    permission.UserPrincipal(creatorID),
    "problem_owner",
    permission.Scope{Type: "problem", ID: problemID},
    nil,
)
```

---

### 21.5 contest-api

后续应接入：

```text
POST /contest/contests
    -> contest.create @ system:0

GET /contest/contests/:id
    -> contest.view @ contest:{id}

PUT /contest/contests/:id
    -> contest.manage @ contest:{id}

POST /contest/contests/:id/problems
    -> contest.manage.problem @ contest:{id}

POST /contest/contests/:id/participants
    -> contest.manage.participant @ contest:{id}

POST /contest/contests/:id/freeze
    -> contest.freeze @ contest:{id}

POST /contest/contests/:id/roll
    -> contest.roll @ contest:{id}
```

比赛创建成功后应自动绑定：

```text
creator -> contest_owner @ contest:{id}
```

比赛添加题目后应写入：

```text
contest:{id} -> problem:{id}
```

到 `resource_edges`。

---

### 21.6 scoreboard-api

后续应接入：

```text
GET /scoreboard/contests/:id
    -> scoreboard.view @ contest:{id}

GET /scoreboard/contests/:id/admin
    -> scoreboard.view.admin @ contest:{id}

POST /scoreboard/contests/:id/freeze
    -> scoreboard.freeze @ contest:{id}

POST /scoreboard/contests/:id/roll
    -> scoreboard.roll @ contest:{id}

GET /scoreboard/contests/:id/export
    -> scoreboard.export @ contest:{id}
```

---

### 21.7 balloon-service

后续应接入：

```text
GET /balloon/contests/:id/tasks
    -> balloon.manage @ contest:{id}

POST /balloon/tasks/:id/deliver
    -> balloon.deliver @ contest:{id}
```

---

### 21.8 print-service

后续应接入：

```text
POST /print/contests/:id/requests
    -> print.request @ contest:{id}

GET /print/contests/:id/requests
    -> print.manage @ contest:{id}

POST /print/requests/:id/operate
    -> print.operate @ contest:{id}
```

---

### 21.9 launcher

后续应接入：

```text
GET /launcher/modules
    -> launcher.view @ system:0

POST /launcher/install
    -> launcher.install @ system:0

POST /launcher/uninstall
    -> launcher.uninstall @ system:0

POST /launcher/enable
    -> launcher.enable @ system:0

POST /launcher/disable
    -> launcher.disable @ system:0
```

---

## 二十二、当前真实验收结果

当前已经真实验证 Permission Core 的基础链路。

测试用户：

```text
permtest
```

角色：

```text
user
```

验证内容：

```text
1. permtest 只有 user 角色
2. 没有 deny 时，permtest 可以提交代码
3. submission 正确写入 user_id
4. judge-worker 正常判题
5. submission 最终 ACCEPTED
6. 写入 judge.submit @ system:0 deny
7. permtest 再提交被 forbidden 拦截
8. 删除 deny
9. permtest 再次提交恢复正常
10. submission 再次 ACCEPTED
```

这说明：

```text
user 角色通过 role_permissions 获得 judge.submit
judge-api 实际调用了 RequireUserPermission
permission_assignments.deny 可以覆盖普通角色权限
删除 deny 后角色权限恢复
```

---

## 二十三、验收 SQL

### 23.1 查看用户角色

```sql
SELECT u.id, u.username, r.name
FROM users u
JOIN user_roles ur ON ur.user_id = u.id
JOIN roles r ON r.id = ur.role_id
WHERE u.username = 'permtest'
ORDER BY r.name;
```

预期：

```text
permtest | user
```

---

### 23.2 写入 deny

```sql
INSERT INTO permission_assignments(
    principal_type,
    principal_id,
    permission_code,
    scope_type,
    scope_id,
    effect,
    reason
)
SELECT
    'user',
    u.id,
    'judge.submit',
    'system',
    0,
    'deny',
    'test deny judge.submit'
FROM users u
WHERE u.username = 'permtest'
ON CONFLICT(principal_type, principal_id, permission_code, scope_type, scope_id)
DO UPDATE SET
    effect = EXCLUDED.effect,
    reason = EXCLUDED.reason;
```

写入后，permtest 提交应被拒绝。

---

### 23.3 查看 deny

```sql
SELECT
    pa.principal_type,
    u.username,
    pa.permission_code,
    pa.scope_type,
    pa.scope_id,
    pa.effect,
    pa.reason
FROM permission_assignments pa
JOIN users u ON u.id = pa.principal_id
WHERE pa.principal_type = 'user'
  AND u.username = 'permtest';
```

预期：

```text
user | permtest | judge.submit | system | 0 | deny
```

---

### 23.4 删除 deny

```sql
DELETE FROM permission_assignments
WHERE principal_type = 'user'
  AND principal_id = (SELECT id FROM users WHERE username = 'permtest')
  AND permission_code = 'judge.submit'
  AND scope_type = 'system'
  AND scope_id = 0;
```

删除后提交应恢复。

---

## 二十四、常见问题

### 24.1 deny 不生效

排查：

```text
1. 用户是否是 super_admin
2. principal_type 是否是 user
3. principal_id 是否正确
4. permission_code 是否正确
5. scope_type / scope_id 是否正确
6. expires_at 是否已过期
7. 业务服务是否真的调用 RequireUserPermission
```

如果用户是 `super_admin`，deny 不生效是设计如此。

---

### 24.2 普通 user 不能提交

排查：

```text
1. permissions 是否有 judge.submit
2. roles 是否有 user
3. role_permissions 是否有 user -> judge.submit
4. user_roles 是否有 当前用户 -> user
5. judge-api 是否传入 system:0
```

SQL：

```sql
SELECT r.name, rp.permission_code
FROM roles r
JOIN role_permissions rp ON rp.role_id = r.id
WHERE r.name = 'user'
ORDER BY rp.permission_code;
```

---

### 24.3 ErrForbidden 现在不是 JSON

当前可能返回：

```text
forbidden
```

这是下一阶段要修的统一错误响应问题。

目标响应：

```json
{
  "code": 40301,
  "msg": "forbidden"
}
```

这属于 HTTP 错误包装，不属于 Permission Core 判断模型本身。

---

### 24.4 资源继承不生效

排查：

```text
1. resource_edges 是否写入
2. parent / child 是否写反
3. relation 是否符合查询逻辑
4. 权限检查是否收集父级 scope
5. role_bindings 是否绑定在父级 scope 上
```

例如：

```text
contest:5 -> problem:7
```

应表示：

```text
contest:5 contains problem:7
```

不要写反。

---

### 24.5 role_permissions 为什么不带 scope

因为角色是能力模板。

例如：

```text
contest_manager
```

角色本身表示：

```text
拥有管理比赛的一组能力
```

至于用户在哪个比赛上拥有这个角色，由：

```text
role_bindings
```

决定。

如果 role_permissions 带 scope，会导致同一个角色在不同资源上重复定义，模型会混乱。

---

## 二十五、安全注意事项

### 25.1 默认拒绝

Permission Core 必须坚持：

```text
默认拒绝
```

没有明确授权就不允许。

不能因为没有 deny 就允许。

---

### 25.2 deny 优先

普通用户的 deny 应覆盖：

```text
user_roles
role_bindings
direct allow
```

但不覆盖：

```text
super_admin
```

---

### 25.3 不在客户端判断权限

前端可以根据权限显示或隐藏按钮，但不能只依赖前端。

后端业务服务必须调用 Permission Core。

---

### 25.4 不在 Gateway 写业务权限

Gateway 不应该硬编码权限点。

否则新增模块会不断修改 Gateway。

---

### 25.5 权限变更应审计

未来所有权限变更都应写入：

```text
permission_audit_logs
```

包括：

```text
绑定角色
撤销角色
直接授权
直接拒绝
删除授权
添加资源关系
删除资源关系
```

---

## 二十六、后续规划

Permission Core 后续需要补：

```text
统一 JSON 错误响应
permission-api
权限管理前端
role revoke
permission revoke
resource edge remove
audit log query
分页查询
权限模板
模块安装时自动注册权限点
模块卸载时权限处理策略
scope inheritance 缓存
权限判断缓存
```

推荐开发顺序：

```text
1. 统一错误响应
2. problem-api 接入 Permission Core
3. 创建 problem 后自动绑定 problem_owner
4. contest-api 接入 Permission Core
5. 创建 contest 后自动绑定 contest_owner
6. 写入 contest -> problem resource_edges
7. permission-api
8. 权限管理 UI
9. module-registry 自动注册 permission / resource_type
```

---

## 二十七、当前结论

Permission Core 当前已经完成 OJOS 从简单角色系统到完整资源级权限系统的基础升级。

当前模型支持：

```text
principal_type / principal_id
scope_type / scope_id
system:0
type:0
resource_edges
allow / deny
super_admin
全局 user_roles
资源级 role_bindings
role_permissions
permission_assignments
permission_audit_logs
```

它已经真实接入：

```text
judge-api POST /judge/submissions
```

并通过：

```text
普通 user allow
直接 deny
删除 deny 恢复
```

完成验证。

后续 OJOS 的所有核心模块都应该接入 Permission Core，包括：

```text
Problem Core
Dataset Core
Contest Core
Scoreboard Core
Balloon
Print
Forum
Clarification
Module Registry
Launcher
```

当前 Permission Core 的正确定位是：

```text
平台内核级授权系统
```

它应该保持稳定，不随业务模块反复重构。

新增模块应通过注册数据扩展 Permission Core，而不是修改 Permission Core 的核心表结构。
