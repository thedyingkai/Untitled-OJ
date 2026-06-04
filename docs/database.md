# Database 文档

## 一、文档定位

本文档记录 OJOS 当前阶段的 PostgreSQL 数据库设计。

OJOS 当前数据库负责保存：

```text
用户身份
角色
资源级权限
题目元数据
题目包路径
提交摘要
提交状态
源码路径
结果路径
取消记录
数据库迁移状态
```

数据库不再保存：

```text
题目测试点输入输出正文
用户提交源码正文
每个测试点完整输出
每个测试点完整 checker 日志
完整评测结果树
```

当前设计原则是：

```text
PostgreSQL 保存事实源和可查询摘要
storage/problems 保存题目包
storage/submissions 保存提交源码和评测产物
result.json 保存完整结构化评测结果
```

也就是说：

```text
数据库是状态事实源
文件系统是大对象和评测产物存储
Redis Streams 是实时任务队列
```

---

## 二、当前数据库

数据库名：

```text
ojos
```

容器内连接：

```text
postgres://postgres:password@postgres:5432/ojos?sslmode=disable
```

宿主机连接：

```text
postgres://postgres:password@localhost:5433/ojos?sslmode=disable
```

进入数据库：

```powershell
docker exec -it ojos-postgres psql -U postgres -d ojos
```

查看表：

```sql
\dt
```

查看表结构：

```sql
\d submissions
\d problems
```

查看迁移版本：

```sql
SELECT * FROM schema_migrations;
```

---

## 三、当前核心表

当前核心表包括：

```text
users
roles
user_roles

resource_types
permissions
role_permissions
role_bindings
permission_assignments
resource_edges
permission_audit_logs

problems
submissions

schema_migrations
```

当前已经删除或废弃：

```text
test_cases
submission_cases
submissions.code
```

说明：

```text
test_cases:
    已被 storage/problems/{id}-{slug}/tests/cases.yaml 和测试数据文件替代

submission_cases:
    已被 storage/submissions/{id}/result.json 替代

submissions.code:
    已被 storage/submissions/{id}/source/* 和 submissions.code_path 替代
```

---

## 四、数据库边界

### 4.1 数据库应该保存什么

数据库应该保存：

```text
用户
角色
权限
资源关系
题目元数据
题目包入口路径
提交摘要
提交状态
提交源码路径
提交结果路径
取消记录
迁移状态
```

例如：

```text
problems.package_dir
submissions.code_path
submissions.result_path
submissions.status
submissions.score
```

这些字段适合查询、过滤、排序和权限判断。

---

### 4.2 数据库不应该保存什么

数据库不应该保存：

```text
完整题面 Markdown 正文
大测试点输入
大测试点答案
用户源码正文
编译日志全文
运行 stdout 全文
运行 stderr 全文
checker 日志全文
完整 case result 数组
```

这些内容应存放在：

```text
storage/problems
storage/submissions
```

原因：

```text
大字段会拖慢数据库
测试数据天然适合文件化
编译和运行产物天然适合文件化
后续支持 SPJ / 子任务 / 交互题时 result 结构会更复杂
```

---

## 五、Auth 相关表

Auth 相关表：

```text
users
roles
user_roles
```

---

## 六、users 表

`users` 保存用户基础身份信息。

典型用途：

```text
登录
注册
JWT 用户 ID
提交归属
权限主体 user:{id}
```

重点字段：

```text
id
username
email
password_hash
created_at
updated_at
```

说明：

| 字段              | 说明          |
| --------------- | ----------- |
| `id`            | 用户 ID       |
| `username`      | 用户名         |
| `email`         | 邮箱          |
| `password_hash` | bcrypt 密码哈希 |
| `created_at`    | 创建时间        |
| `updated_at`    | 更新时间        |

当前 Auth 模块负责：

```text
用户注册
用户登录
密码校验
JWT 签发
默认 user 角色绑定
```

---

## 七、roles 表

`roles` 保存角色定义。

角色是权限集合模板，本身不包含资源作用域。

重点字段：

```text
id
name
module_code
description
is_system
created_at
```

常见角色：

```text
super_admin
admin
user

problem_owner
problem_setter
problem_viewer
problem_data_manager

contest_owner
contest_manager
contest_judge
contest_participant
```

说明：

```text
系统级角色通常通过 user_roles 绑定
资源级角色通常通过 role_bindings 绑定
```

---

## 八、user_roles 表

`user_roles` 保存用户的系统级全局角色。

字段：

```text
user_id
role_id
```

用途：

```text
给用户绑定全局角色
例如普通注册用户拥有 user 角色
例如管理员拥有 admin 或 super_admin 角色
```

示例：

```text
user:2 -> user
user:1 -> super_admin
```

如果：

```text
user 角色拥有 judge.submit
```

并且：

```text
user:2 -> user
```

则用户 2 默认可以：

```text
judge.submit @ system:0
```

---

## 九、Permission Core 相关表

Permission Core 相关表：

```text
resource_types
permissions
role_permissions
role_bindings
permission_assignments
resource_edges
permission_audit_logs
```

这些表实现资源级权限系统。

核心模型：

```text
Can(principal, permission, scope)
```

例如：

```text
Can(user:2, "judge.submit", system:0)
Can(user:3, "problem.manage.data", problem:7)
Can(user:4, "contest.manage", contest:5)
```

---

## 十、resource_types 表

`resource_types` 是资源类型注册表。

用途：

```text
声明系统中有哪些资源类型
支持模块注册自己的资源类型
避免在代码中写死资源类型 enum
```

典型资源类型：

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

重点字段：

```text
code
module_code
name
description
created_at
```

示例：

```sql
INSERT INTO resource_types(code, module_code, name, description)
VALUES
    ('problem', 'problem-core', 'Problem', '题目资源'),
    ('contest', 'contest-core', 'Contest', '比赛资源'),
    ('submission', 'judge-core', 'Submission', '提交资源')
ON CONFLICT(code) DO NOTHING;
```

---

## 十一、permissions 表

`permissions` 是权限点注册表。

用途：

```text
声明系统中有哪些权限点
支持模块注册自己的权限点
避免在代码中写死权限点 enum
```

重点字段：

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
submission.cancel
submission.delete

contest.create
contest.view
contest.manage
contest.freeze
contest.roll
```

示例：

```sql
INSERT INTO permissions(code, module_code, name, description)
VALUES
    ('judge.submit', 'judge-core', 'Submit Code', '提交代码'),
    ('problem.manage.data', 'problem-core', 'Manage Problem Data', '管理题目数据'),
    ('submission.view.own', 'judge-core', 'View Own Submission', '查看自己的提交')
ON CONFLICT(code) DO NOTHING;
```

---

## 十二、role_permissions 表

`role_permissions` 表示某个角色拥有哪些权限点。

字段：

```text
role_id
permission_code
created_at
```

推荐唯一约束：

```text
(role_id, permission_code)
```

说明：

```text
role_permissions 不带 scope
```

原因：

```text
角色只是能力模板
用户在哪个资源上拥有这个角色，由 user_roles 或 role_bindings 决定
```

示例：

```sql
INSERT INTO role_permissions(role_id, permission_code)
SELECT r.id, 'judge.submit'
FROM roles r
WHERE r.name = 'user'
ON CONFLICT DO NOTHING;
```

含义：

```text
user 角色拥有 judge.submit 权限模板
```

---

## 十三、role_bindings 表

`role_bindings` 是资源级角色绑定表。

用途：

```text
声明某个权限主体在某个资源范围内拥有某个角色
```

重点字段：

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

示例：

```text
user:2 -> problem_owner @ problem:7
user:3 -> contest_manager @ contest:5
team:9 -> contest_participant @ contest:5
```

解释：

```text
用户 2 是题目 7 的 owner
用户 3 是比赛 5 的管理员
队伍 9 是比赛 5 的参赛队伍
```

创建题目后，后续应自动写入：

```text
creator -> problem_owner @ problem:{id}
```

当前如果还没做自动绑定，需要后续补。

---

## 十四、permission_assignments 表

`permission_assignments` 是直接授权 / 直接拒绝表。

用途：

```text
临时授权
临时禁止
封禁用户
特殊权限例外
覆盖普通角色权限
```

重点字段：

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

`effect` 可选：

```text
allow
deny
```

示例：

```text
allow user:5 problem.edit @ problem:9
deny  user:7 judge.submit @ system:0
allow user:8 scoreboard.roll @ contest:5
```

当前已验证过：

```text
deny user:permtest judge.submit @ system:0
```

写入后，普通用户提交会被 forbidden 拦截；删除 deny 后，提交恢复。

规则：

```text
super_admin 最高优先
deny 优先于普通 allow 和角色权限
allow 优先于默认拒绝
默认拒绝
```

---

## 十五、resource_edges 表

`resource_edges` 表示资源继承关系。

用途：

```text
让权限可以沿资源父子关系继承
```

重点字段：

```text
id
parent_type
parent_id
child_type
child_id
relation
created_at
```

示例：

```text
contest:3 -> problem:7
contest:3 -> submission:100
group:1   -> contest:3
```

含义：

```text
problem:7 属于 contest:3
submission:100 属于 contest:3
contest:3 属于 group:1
```

这样后续可以支持：

```text
用户是 contest:3 的 contest_manager
因此可以管理 contest:3 下的 submission
```

当前后续需要补：

```text
创建 contest/problem 关系时自动写 resource_edges
创建 submission 时自动写 contest -> submission 关系
```

如果比赛系统未实现，当前可以先不写复杂 edges。

---

## 十六、permission_audit_logs 表

`permission_audit_logs` 是权限审计日志表。

用途：

```text
记录权限变更历史
记录角色绑定历史
记录直接授权或拒绝历史
支持后台追踪
支持安全审计
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

后续缺口：

```text
审计日志查询 API
权限后台 UI
分页和筛选
```

---

## 十七、Problem 相关表

当前 Problem 相关核心表：

```text
problems
```

当前不再使用：

```text
test_cases
```

题目测试数据已经迁移到：

```text
storage/problems/{id}-{slug}/
```

---

## 十八、problems 表

`problems` 保存题目元数据和题目包入口。

重点字段：

```text
id
slug
title
statement
tutorial
time_limit_ms
memory_limit_mb
package_dir
visibility
status
owner_id
created_at
updated_at
```

说明：

| 字段                | 说明        |
| ----------------- | --------- |
| `id`              | 题目 ID     |
| `slug`            | 题目短标识     |
| `title`           | 题目标题      |
| `statement`       | 题面摘要或兼容字段 |
| `tutorial`        | 题解摘要或兼容字段 |
| `time_limit_ms`   | 默认时间限制，毫秒 |
| `memory_limit_mb` | 默认内存限制，MB |
| `package_dir`     | 题目包目录     |
| `visibility`      | 可见性       |
| `status`          | 题目状态      |
| `owner_id`        | 所有者用户 ID  |
| `created_at`      | 创建时间      |
| `updated_at`      | 更新时间      |

其中最关键的是：

```text
package_dir
```

它指向题目包目录，例如：

```text
/data/ojos/problems/2-a-plus-b
```

Judge Worker 通过该字段读取：

```text
problem.yaml
tests/cases.yaml
tests/*.in
tests/*.ans
```

---

## 十九、problems.package_dir

`package_dir` 是题目包入口路径。

示例：

```text
/data/ojos/problems/2-a-plus-b
```

对应宿主机路径：

```text
D:\Untitled-OJ\storage\problems\2-a-plus-b
```

注意：

```text
数据库中保存容器内路径
```

原因：

```text
problem-api / judge-worker 都运行在 Docker 容器内
服务内部直接使用 /data/ojos/problems 路径
```

后续如果引入对象存储，可以改为逻辑 URI：

```text
storage://problems/2-a-plus-b
```

当前先使用本地挂载路径。

---

## 二十、为什么删除 test_cases

旧设计：

```text
test_cases
    id
    problem_id
    input
    output
    score
    created_at
```

当前废弃。

原因：

```text
测试数据可能很大
测试数据需要文件名和目录结构
后续需要支持多文件测试
后续需要支持子任务 / 捆绑点
后续需要支持 SPJ / 交互题
数据库 TEXT 字段不适合保存大 input/output
题目包更适合导入导出和版本管理
```

当前替代方案：

```text
storage/problems/{id}-{slug}/tests/cases.yaml
storage/problems/{id}-{slug}/tests/*.in
storage/problems/{id}-{slug}/tests/*.ans
```

---

## 二十一、Submission 相关表

当前 Submission 相关核心表：

```text
submissions
```

当前不再使用：

```text
submission_cases
```

完整 case 结果已经迁移到：

```text
storage/submissions/{submission_id}/result.json
```

---

## 二十二、submissions 表

`submissions` 保存提交摘要、状态和文件路径。

重点字段：

```text
id
problem_id
user_id
language
status
score
time_ms
memory_kb
message
code_path
code_sha256
result_path
judged_at
cancelled_at
cancelled_by
cancel_reason
created_at
updated_at
```

说明：

| 字段              | 说明             |
| --------------- | -------------- |
| `id`            | 提交 ID          |
| `problem_id`    | 题目 ID          |
| `user_id`       | 提交用户 ID        |
| `language`      | 提交语言           |
| `status`        | 提交状态           |
| `score`         | 总分             |
| `time_ms`       | 时间摘要           |
| `memory_kb`     | 内存摘要，当前暂为 0    |
| `message`       | 摘要消息           |
| `code_path`     | 源码文件路径         |
| `code_sha256`   | 源码 SHA-256     |
| `result_path`   | result.json 路径 |
| `judged_at`     | 最近评测完成时间       |
| `cancelled_at`  | 取消时间           |
| `cancelled_by`  | 取消操作者          |
| `cancel_reason` | 取消原因           |
| `created_at`    | 创建时间           |
| `updated_at`    | 更新时间           |

---

## 二十三、submissions.status

当前支持的状态：

```text
PENDING
JUDGING
ACCEPTED
WRONG_ANSWER
COMPILE_ERROR
RUNTIME_ERROR
TIME_LIMIT_EXCEEDED
SYSTEM_ERROR
UNSUPPORTED_LANGUAGE
CANCELLED
```

状态流转：

```text
PENDING
  ↓
JUDGING
  ↓
ACCEPTED / WRONG_ANSWER / COMPILE_ERROR / RUNTIME_ERROR
/ TIME_LIMIT_EXCEEDED / SYSTEM_ERROR / UNSUPPORTED_LANGUAGE
```

Cancel：

```text
任意已存在提交
  ↓
CANCELLED
```

Rejudge：

```text
该题全部提交，包括 CANCELLED
  ↓
PENDING
  ↓
重新评测
```

---

## 二十四、submissions.code_path

`code_path` 指向用户源码文件。

示例：

```text
/data/ojos/submissions/20/source/main.cpp
```

对应宿主机路径：

```text
D:\Untitled-OJ\storage\submissions\20\source\main.cpp
```

当前不再使用：

```text
submissions.code
```

保存源码正文。

---

## 二十五、submissions.code_sha256

`code_sha256` 保存源码内容 SHA-256。

用途：

```text
源码完整性校验
辅助排查重复提交
后续编译缓存
后续审计
```

当前它不是唯一约束。

---

## 二十六、submissions.result_path

`result_path` 指向完整评测结果文件。

示例：

```text
/data/ojos/submissions/20/result.json
```

`GET /judge/submissions/:id/cases` 当前读取：

```text
submissions.result_path
```

然后解析：

```text
result.json
```

返回 case 结果。

---

## 二十七、为什么删除 submission_cases

旧设计：

```text
submission_cases
    id
    submission_id
    test_case_id
    status
    time_ms
    memory_kb
    message
    created_at
```

当前废弃。

原因：

```text
完整 case 结果结构会越来越复杂
stdout / stderr / checker.log 不适合进数据库
后续子任务 / 捆绑点 / SPJ / 交互题需要树状结果
result.json 更适合保存完整结构化结果
数据库只需要保存可查询摘要
```

当前替代方案：

```text
storage/submissions/{id}/result.json
storage/submissions/{id}/cases/{case_no}/stdout.txt
storage/submissions/{id}/cases/{case_no}/stderr.txt
storage/submissions/{id}/cases/{case_no}/checker.log
```

---

## 二十八、索引建议

### 28.1 submissions 索引

建议保留：

```sql
CREATE INDEX IF NOT EXISTS idx_submissions_problem_id
    ON submissions(problem_id);

CREATE INDEX IF NOT EXISTS idx_submissions_user_id
    ON submissions(user_id);

CREATE INDEX IF NOT EXISTS idx_submissions_status
    ON submissions(status);

CREATE INDEX IF NOT EXISTS idx_submissions_problem_status
    ON submissions(problem_id, status);

CREATE INDEX IF NOT EXISTS idx_submissions_user_status
    ON submissions(user_id, status);
```

如果保留 `code_sha256` 查询能力，可以保留：

```sql
CREATE INDEX IF NOT EXISTS idx_submissions_code_sha256
    ON submissions(code_sha256);
```

如果经常查询取消记录，可以保留：

```sql
CREATE INDEX IF NOT EXISTS idx_submissions_cancelled_at
    ON submissions(cancelled_at);
```

如果经常按评测完成时间查询，可以保留：

```sql
CREATE INDEX IF NOT EXISTS idx_submissions_judged_at
    ON submissions(judged_at);
```

---

### 28.2 problems 索引

建议：

```sql
CREATE UNIQUE INDEX IF NOT EXISTS idx_problems_slug
    ON problems(slug);

CREATE INDEX IF NOT EXISTS idx_problems_owner_id
    ON problems(owner_id);

CREATE INDEX IF NOT EXISTS idx_problems_visibility
    ON problems(visibility);

CREATE INDEX IF NOT EXISTS idx_problems_status
    ON problems(status);
```

---

### 28.3 Permission Core 索引

建议：

```sql
CREATE UNIQUE INDEX IF NOT EXISTS idx_resource_types_code
    ON resource_types(code);

CREATE UNIQUE INDEX IF NOT EXISTS idx_permissions_code
    ON permissions(code);

CREATE UNIQUE INDEX IF NOT EXISTS idx_role_permissions_unique
    ON role_permissions(role_id, permission_code);

CREATE INDEX IF NOT EXISTS idx_role_bindings_principal
    ON role_bindings(principal_type, principal_id);

CREATE INDEX IF NOT EXISTS idx_role_bindings_scope
    ON role_bindings(scope_type, scope_id);

CREATE INDEX IF NOT EXISTS idx_permission_assignments_principal
    ON permission_assignments(principal_type, principal_id);

CREATE INDEX IF NOT EXISTS idx_permission_assignments_scope
    ON permission_assignments(scope_type, scope_id);

CREATE INDEX IF NOT EXISTS idx_resource_edges_child
    ON resource_edges(child_type, child_id);

CREATE INDEX IF NOT EXISTS idx_resource_edges_parent
    ON resource_edges(parent_type, parent_id);
```

具体名称以 migration 实际文件为准。

---

## 二十九、外键关系建议

### 29.1 problems

建议：

```text
problems.owner_id -> users.id
```

删除策略：

```text
ON DELETE SET NULL
```

或：

```text
ON DELETE RESTRICT
```

不建议用户删除时级联删除题目。

---

### 29.2 submissions

建议：

```text
submissions.problem_id -> problems.id
submissions.user_id -> users.id
submissions.cancelled_by -> users.id
```

删除策略：

```text
problem_id:
    ON DELETE CASCADE 或 RESTRICT，取决于删除题目是否删除提交

user_id:
    ON DELETE RESTRICT 或 SET NULL，取决于是否允许删除用户

cancelled_by:
    ON DELETE SET NULL
```

当前开发阶段可以使用：

```text
problem_id REFERENCES problems(id) ON DELETE CASCADE
cancelled_by REFERENCES users(id) ON DELETE SET NULL
```

但生产阶段应重新讨论题目删除和提交保留策略。

---

## 三十、schema_migrations 表

`schema_migrations` 由 `golang-migrate` 管理。

用途：

```text
记录当前数据库迁移版本
记录 dirty 状态
```

查看：

```sql
SELECT * FROM schema_migrations;
```

不要手动修改，除非明确知道 dirty 状态修复方式。

---

## 三十一、Migration 规则

迁移目录：

```text
deploy/migrations
```

执行迁移：

```powershell
cd D:\Untitled-OJ

migrate `
  -path deploy/migrations `
  -database "postgres://postgres:password@localhost:5433/ojos?sslmode=disable" `
  up
```

回滚一步：

```powershell
migrate `
  -path deploy/migrations `
  -database "postgres://postgres:password@localhost:5433/ojos?sslmode=disable" `
  down 1
```

新建迁移：

```powershell
migrate create `
  -ext sql `
  -dir deploy/migrations `
  -seq <migration_name>
```

原则：

```text
已执行过并提交的 migration 不要随意改历史
新增结构用新的 migration
down 文件要谨慎
权限点注册要可重复执行
资源类型注册要可重复执行
初始化数据使用 ON CONFLICT DO NOTHING
```

开发阶段如果 migration 写错且已经本地执行：

```text
优先写新的修复 migration
不要在数据库状态不清楚时反复手改旧 migration
```

---

## 三十二、当前应确认删除的旧结构

当前应确认不存在：

```text
test_cases
submission_cases
```

检查：

```sql
SELECT to_regclass('public.test_cases') AS test_cases;
SELECT to_regclass('public.submission_cases') AS submission_cases;
```

预期：

```text
null
null
```

当前应确认 `submissions` 不存在：

```text
code
```

检查：

```sql
SELECT column_name
FROM information_schema.columns
WHERE table_name = 'submissions'
ORDER BY ordinal_position;
```

预期应有：

```text
code_path
code_sha256
result_path
```

不应有：

```text
code
```

如果仍然有 `code`，说明旧 migration 或回滚没有清理干净。

---

## 三十三、常用排查 SQL

### 33.1 查看最近提交

```sql
SELECT
    id,
    problem_id,
    user_id,
    language,
    status,
    score,
    time_ms,
    memory_kb,
    message,
    code_path,
    result_path,
    judged_at,
    cancelled_at,
    cancel_reason
FROM submissions
ORDER BY id DESC
LIMIT 20;
```

---

### 33.2 查看 PENDING

```sql
SELECT
    id,
    problem_id,
    user_id,
    language,
    status,
    created_at,
    updated_at
FROM submissions
WHERE status = 'PENDING'
ORDER BY id;
```

---

### 33.3 查看 JUDGING

```sql
SELECT
    id,
    problem_id,
    user_id,
    language,
    status,
    created_at,
    updated_at
FROM submissions
WHERE status = 'JUDGING'
ORDER BY id;
```

---

### 33.4 手动恢复开发环境卡住的 JUDGING

开发环境可以使用：

```sql
UPDATE submissions
SET status = 'PENDING',
    updated_at = NOW()
WHERE id = 20
  AND status = 'JUDGING';
```

生产环境不应随意手动修改，需要审计和重测机制。

---

### 33.5 查看题目包路径

```sql
SELECT
    id,
    slug,
    title,
    package_dir,
    visibility,
    status,
    owner_id
FROM problems
ORDER BY id;
```

---

### 33.6 查看用户角色

```sql
SELECT
    u.id,
    u.username,
    r.name
FROM users u
JOIN user_roles ur ON ur.user_id = u.id
JOIN roles r ON r.id = ur.role_id
ORDER BY u.id, r.name;
```

---

### 33.7 查看权限点

```sql
SELECT
    code,
    module_code,
    name,
    description
FROM permissions
ORDER BY code;
```

---

### 33.8 查看某用户直接授权 / 拒绝

```sql
SELECT
    pa.*
FROM permission_assignments pa
WHERE pa.principal_type = 'user'
  AND pa.principal_id = 2
ORDER BY pa.created_at DESC;
```

---

## 三十四、与 Redis Streams 的关系

数据库和 Redis Streams 分工：

```text
PostgreSQL:
    保存提交状态事实源

Redis Streams:
    保存实时判题任务队列
```

提交创建时：

```text
judge-api INSERT submissions(status=PENDING)
judge-api XADD ojos:judge:submissions
```

Worker 消费时：

```text
XREADGROUP 收到 submission_id
UPDATE submissions SET status='JUDGING'
WHERE id=? AND status='PENDING'
RETURNING id
```

只有数据库抢任务成功，worker 才能判题。

原因：

```text
Redis 历史消息可能重复出现
PENDING 扫描可能和 Redis 消费并发
rejudge 会重新投递任务
数据库状态机才是防重复执行核心
```

---

## 三十五、当前数据库与文件系统的关系

### 35.1 Problem

数据库：

```text
problems.package_dir
```

指向：

```text
storage/problems/{id}-{slug}/
```

例如：

```text
/data/ojos/problems/2-a-plus-b
```

题目包内保存：

```text
problem.yaml
tests/cases.yaml
tests/*.in
tests/*.ans
statement/*.md
tutorial/*.md
runner/runner.yaml
checker/checker.yaml
scorer/scorer.yaml
```

---

### 35.2 Submission

数据库：

```text
submissions.code_path
submissions.result_path
```

指向：

```text
storage/submissions/{submission_id}/source/*
storage/submissions/{submission_id}/result.json
```

提交目录内保存：

```text
source/*
build/compile.log
cases/{case_no}/stdout.txt
cases/{case_no}/stderr.txt
cases/{case_no}/checker.log
result.json
```

---

## 三十六、当前已知限制

当前数据库设计仍有以下限制：

```text
没有 problem_versions
没有 problem_tags
没有 problem_permissions 专表
没有 dataset_versions
没有 judge_runs
没有 rejudge history
没有 submission result version
没有 contest 表
没有 contest_problem 表
没有 scoreboard 表
没有 team 表
没有 permission-api 管理表分页视图
```

当前开发阶段可以接受。

不要现在为了“看起来完整”提前加大量表。

下一阶段优先级：

```text
Problem / Dataset 模型稳定
Runner / Checker / Scorer 结果模型稳定
memory_kb 统计
多语言验收
```

之后再进入：

```text
Contest Core
Scoreboard Core
Team Core
Module Registry
```

---

## 三十七、后续演进方向

数据库后续会逐步增加：

```text
problem_versions
problem_statements
problem_tags
problem_owners
dataset_versions
dataset_files
judge_runs
contest
contest_problems
contest_participants
teams
scoreboard_snapshots
clarifications
balloons
prints
modules
feature_flags
```

但这些必须在对应模块设计稳定后再加。

当前不要把所有未来表一次性塞进数据库。

原则：

```text
当前能支撑真实功能
当前能清晰表达边界
当前能避免旧结构债务
当前不为未实现模块过度建表
```

---

## 三十八、当前结论

当前 OJOS 数据库已经从早期 MVP 结构：

```text
problems
test_cases
submissions
submission_cases
```

升级为：

```text
users
roles
user_roles

resource_types
permissions
role_permissions
role_bindings
permission_assignments
resource_edges
permission_audit_logs

problems
submissions
```

其中：

```text
题目数据文件化到 storage/problems
提交源码和结果文件化到 storage/submissions
完整 case 结果进入 result.json
数据库只保留元数据、状态、摘要和路径
```

当前最重要的数据库边界是：

```text
PostgreSQL 是事实源
Redis Streams 是队列
storage 是大对象和评测产物
```

不要再恢复：

```text
test_cases
submission_cases
submissions.code
```

后续扩展应围绕：

```text
Problem Core
Dataset Core
Judge Runs
Contest Core
Scoreboard Core
Permission API
```

逐步新增表，而不是回退到旧的 MVP 表结构。
