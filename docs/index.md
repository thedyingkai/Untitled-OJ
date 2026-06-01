# OJOS 文档索引

## 一、文档定位

本文档是 OJOS 项目 `docs/` 目录的总入口。

它用于说明：

```text
当前有哪些文档
每份文档负责什么内容
应该按什么顺序阅读
当前项目处于什么阶段
哪些内容已经完成
哪些内容尚未完成
后续新增模块时应该补充哪些文档
```

OJOS 当前已经不是单个服务的小项目，而是一个包含：

```text
Go 微服务
Rust Judge Worker
Vue / Vite 前端
PostgreSQL migrations
Redis Streams
Docker Compose
Permission Core
Gateway
Auth
Judge API
Judge Worker
```

的 Monorepo 项目。

因此文档不能只靠 README 一个文件维护。

当前文档采用：

```text
README.md
+
docs/*.md
```

的形式组织。

其中：

```text
README.md
```

负责项目总体介绍、快速启动、当前状态总览。

`docs/` 目录负责详细模块文档、架构说明、开发流程和后续规划。

---

## 二、当前文档列表

当前建议维护以下文档：

```text
docs/index.md
docs/architecture_overview.md
docs/shared_module.md
docs/auth_module.md
docs/gateway_module.md
docs/permission_core_module.md
docs/judge_module.md
docs/judge_worker_module.md
docs/development_workflow.md
```

每份文档职责如下。

| 文档                               | 职责                      |
| -------------------------------- | ----------------------- |
| `docs/index.md`                  | 文档总入口，说明阅读顺序和文档结构       |
| `docs/architecture_overview.md`  | 整体架构、模块边界、长期方向          |
| `docs/shared_module.md`          | Shared 公共 Go 基础库说明      |
| `docs/auth_module.md`            | Auth 认证服务说明             |
| `docs/gateway_module.md`         | Gateway 网关服务说明          |
| `docs/permission_core_module.md` | 资源级权限核心说明               |
| `docs/judge_module.md`           | Judge API 与 Judge 总链路说明 |
| `docs/judge_worker_module.md`    | Rust Judge Worker 详细说明  |
| `docs/development_workflow.md`   | 本地开发、生成、构建、Git、验收流程     |

根目录还应保留：

```text
README.md
```

它负责项目首页级介绍。

---

## 三、推荐阅读顺序

如果是第一次阅读 OJOS 项目，建议按以下顺序阅读。

### 3.1 快速了解项目

先读：

```text
README.md
```

作用：

```text
了解项目定位
了解当前技术栈
了解当前模块
了解快速启动方式
了解当前完成情况
了解下一阶段计划
```

README 不应该塞入所有实现细节。

README 的定位是：

```text
项目入口
```

不是：

```text
所有模块的详细说明书
```

---

### 3.2 理解总体架构

然后读：

```text
docs/architecture_overview.md
```

作用：

```text
理解 OJOS 为什么不是普通 OJ
理解 Kernel + Modules 架构
理解 Gateway / Auth / Permission / Judge 的边界
理解 NATS 为什么被 Redis Streams 替换
理解 PostgreSQL 为什么是事实源
理解未来 Problem / Dataset / Contest / Module Registry 的位置
```

如果要继续参与系统设计，必须读这份文档。

---

### 3.3 理解公共基础库

然后读：

```text
docs/shared_module.md
```

作用：

```text
理解 services/shared 的定位
理解 shared 不是独立服务
理解 shared 不再包含 config / response / events
理解 shared 中 database / logger / middleware / tracing / jwt / authctx / permission 的职责
理解为什么不要过早恢复 EventBus
```

新增 Go 服务时，必须参考这份文档接入 Shared。

---

### 3.4 理解认证链路

然后读：

```text
docs/auth_module.md
```

作用：

```text
理解用户注册
理解用户登录
理解 bcrypt
理解 JWT 签发
理解默认 user 角色绑定
理解 Auth 与 Permission Core 的边界
理解 Auth 不再发布 NATS 事件
```

如果要改登录、注册、Profile、Token、用户角色，先读这份。

---

### 3.5 理解入口层

然后读：

```text
docs/gateway_module.md
```

作用：

```text
理解 Gateway 统一入口
理解配置驱动代理
理解 AuthMode
理解 JWT 解析
理解可信用户 Header 注入
理解为什么 Gateway 不做业务权限判断
理解如何接入新服务路由
```

新增 `problem-api / contest-api / scoreboard-api` 时，必须参考 Gateway 文档配置路由。

---

### 3.6 理解资源级权限

然后读：

```text
docs/permission_core_module.md
```

作用：

```text
理解完整资源级权限模型
理解 principal / permission / scope
理解 role_permissions / role_bindings / permission_assignments
理解 deny / allow / super_admin 优先级
理解 resource_edges 资源继承
理解业务服务如何调用 RequireUserPermission
```

后续所有业务模块都必须接入 Permission Core。

---

### 3.7 理解 Judge 总链路

然后读：

```text
docs/judge_module.md
```

作用：

```text
理解 judge-api 的职责
理解 submissions / submission_cases
理解 Redis Streams Judge Queue
理解 judge.submit 权限检查
理解当前 Judge API 中 problem / test-case 只是 MVP
理解后续应拆出 Problem Core / Dataset Core
```

如果要改提交、查询结果、Redis XADD、Judge API，先读这份。

---

### 3.8 理解 Rust Worker

然后读：

```text
docs/judge_worker_module.md
```

作用：

```text
理解 judge-worker 如何消费 Redis Stream
理解 XREADGROUP / XACK
理解 PENDING 扫描
理解 try_claim_submission
理解 languages.yaml
理解编译运行流程
理解当前安全限制
理解后续 Runner Core 方向
```

如果要改判题执行、语言配置、Redis 消费、PENDING 恢复，先读这份。

---

### 3.9 理解开发流程

最后读：

```text
docs/development_workflow.md
```

作用：

```text
理解 Git 怎么管理生成文件
理解 go-zero 怎么生成
理解 GoLand 怎么配置自动生成
理解 Docker Compose 怎么启动
理解 migrations 怎么跑
理解 NATS 清理怎么确认
理解 Redis Streams 怎么验收
理解提交前要做哪些检查
```

如果要继续开发任何模块，都应该熟悉这份文档。

---

## 四、当前系统状态摘要

当前 OJOS 已完成第一阶段基础闭环。

已经完成：

```text
Docker Compose 本地编排
PostgreSQL
Redis
Jaeger
Gateway
Auth
Shared
Permission Core
Judge API
Judge Worker
Redis Streams Judge Queue
JWT 鉴权
Gateway 用户上下文透传
judge.submit 权限检查
PENDING 兜底扫描
数据库原子抢任务
Redis XACK
多语言基础评测
```

已经移除：

```text
NATS
NATS Core Pub/Sub Judge Queue
shared/events
shared/events/nats.go
judge-worker/src/event.rs
async-nats
NATS_URL
```

当前可跑通：

```text
用户注册
用户登录
获取 JWT
Gateway 解析 JWT
Gateway 注入可信用户上下文
Judge API 读取 user_id
Permission Core 检查 judge.submit
创建 submission
Redis Streams 投递任务
Judge Worker 消费任务
Judge Worker 编译运行
Judge Worker 写入结果
用户查询结果
```

当前已经验证：

```text
普通 user 可以提交
deny judge.submit 后禁止提交
删除 deny 后恢复提交
Redis XPENDING 为 0
submission 最终 ACCEPTED
```

---

## 五、当前还不是生产级的部分

当前系统仍然不是生产级完整 OJ。

主要缺口：

```text
统一 JSON 错误响应尚未完成
Problem Core 尚未完成
Dataset Core 尚未完成
Runner Core 尚未完成
安全沙箱尚未完成
测试数据文件化尚未完成
Special Judge 尚未完成
子任务 / 捆绑点尚未完成
交互题尚未完成
通信题尚未完成
提交答案题尚未完成
Contest Core 尚未完成
Scoreboard Core 尚未完成
Module Registry 尚未完成
Feature Flag Core 尚未完成
Launcher 尚未完成
Permission 管理 API / UI 尚未完成
```

其中最重要的风险是：

```text
Judge Worker 当前没有安全沙箱
```

当前用户代码仍然直接运行在 judge-worker 容器中。

因此当前系统适合：

```text
本地开发
架构验证
可信环境测试
MVP 功能演示
```

不适合：

```text
公网开放
陌生用户提交
正式比赛
生产部署
```

---

## 六、当前模块完成情况

| 模块              | 当前状态               |
| --------------- | ------------------ |
| Docker Compose  | 已完成基础可运行           |
| PostgreSQL      | 已完成                |
| Redis           | 已完成                |
| Jaeger          | 已完成                |
| NATS            | 已移除                |
| Shared          | v0.3+              |
| Auth            | v0.2+              |
| Gateway         | v0.3+              |
| Permission Core | v1                 |
| Judge API       | MVP v0.3+          |
| Judge Worker    | Reliability v0.3   |
| Judge Queue     | Redis Streams v0.3 |
| Frontend        | 初始框架               |
| Problem Core    | 未完成                |
| Dataset Core    | 未完成                |
| Runner Core     | 未完成                |
| Contest Core    | 未完成                |
| Scoreboard Core | 未完成                |
| Module Registry | 未完成                |
| Launcher        | 未完成                |

---

## 七、文档维护原则

OJOS 文档必须和代码同步维护。

每次完成以下变更时，都应该更新相关文档：

```text
新增服务
删除服务
新增数据库表
修改数据库表
新增权限点
新增资源类型
修改 Gateway 路由
修改 Auth 流程
修改 Judge 队列
修改 Worker 消费模型
修改 Docker Compose
修改 go-zero API
修改环境变量
修改启动方式
修改验收命令
```

例如：

```text
NATS -> Redis Streams
```

这类架构变化必须更新：

```text
README.md
docs/architecture_overview.md
docs/judge_module.md
docs/judge_worker_module.md
docs/development_workflow.md
```

如果只改代码不改文档，后续会出现：

```text
文档仍写 NATS
代码已经用 Redis
新人按文档调试失败
服务配置互相矛盾
重复踩坑
```

---

## 八、新增模块时应补哪些文档

后续新增模块时，应该同步新增模块文档。

例如新增：

```text
problem-api
```

应新增：

```text
docs/problem_module.md
```

内容至少包括：

```text
模块定位
当前版本状态
目录结构
API 列表
配置文件
数据库表
Permission Core 接入
Gateway 路由
与 Dataset Core 的关系
与 Judge API 的关系
验收命令
常见问题
后续规划
```

新增：

```text
contest-core
```

应新增：

```text
docs/contest_module.md
```

新增：

```text
scoreboard-core
```

应新增：

```text
docs/scoreboard_module.md
```

新增：

```text
module-registry
```

应新增：

```text
docs/module_registry.md
```

新增：

```text
launcher
```

应新增：

```text
docs/launcher_module.md
```

---

## 九、推荐文档命名规范

模块文档命名：

```text
<module>_module.md
```

例如：

```text
auth_module.md
gateway_module.md
shared_module.md
judge_module.md
judge_worker_module.md
permission_core_module.md
problem_module.md
contest_module.md
scoreboard_module.md
launcher_module.md
```

架构类文档命名：

```text
architecture_overview.md
development_workflow.md
deployment.md
database_schema.md
security_model.md
```

不要使用含糊名称，例如：

```text
new_doc.md
note.md
todo.md
temp.md
final.md
```

---

## 十、文档内容规范

每份模块文档建议包含以下结构：

```text
一、模块定位
二、当前版本状态
三、目录结构
四、配置文件
五、数据库结构
六、核心流程
七、API 文档
八、权限接入
九、与其他模块关系
十、Docker / 启动方式
十一、验收命令
十二、常见问题
十三、安全注意事项
十四、后续规划
十五、当前结论
```

并且应该明确：

```text
当前已经完成什么
当前没有完成什么
哪些是 MVP 临时设计
哪些是长期设计
哪些接口后续要迁移
哪些模块已经删除
哪些依赖不应该再出现
```

文档不应该只写一句：

```text
本模块负责用户认证
```

这种内容没有维护价值。

---

## 十一、当前关键架构约定

### 11.1 NATS 已移除

当前架构中不再使用 NATS。

如果文档中出现：

```text
NATS
NatsConfig
NATS_URL
nats://ojos-nats:4222
```

除非是在“历史说明 / 已删除内容”中，否则应视为过时内容。

当前 Judge Queue 是：

```text
Redis Streams
```

---

### 11.2 Gateway 不做业务授权

Gateway 负责：

```text
JWT 验证
Header 注入
代理
```

业务服务负责：

```text
Permission Core 权限检查
```

不要在 Gateway 中硬编码：

```text
judge.submit
problem.edit
contest.manage
module.install
```

---

### 11.3 PostgreSQL 是事实源

Redis 可以用于：

```text
队列
缓存
临时状态
限流
```

但核心业务状态必须落 PostgreSQL。

例如 Judge：

```text
Redis Stream 是实时任务队列
submissions 表是最终状态来源
```

---

### 11.4 Shared 不放业务逻辑

Shared 可以放：

```text
database
logger
middleware
tracing
jwt
authctx
permission
```

不应放：

```text
problem logic
contest logic
judge logic
scoreboard logic
module install logic
```

---

### 11.5 生成文件也要进 Git

go-zero 生成的：

```text
handler
logic
types
routes
```

属于源码，应该提交。

不要因为它们是生成文件就不管理。

---

## 十二、当前下一阶段计划

当前推荐下一阶段开发顺序：

```text
1. 确认 NATS 残留全部清理干净
2. 确认当前文档全部更新完成
3. 统一错误响应，尤其是 forbidden -> JSON
4. Problem Core / Dataset Core 正规化
5. problem-api 接入 Permission Core
6. 创建 problem 后自动绑定 problem_owner
7. 将 judge-api 中的 problem / test-case 管理接口迁移到 problem-api
8. Runner Core 抽象与基础安全隔离
9. 测试数据文件化
10. checker / special judge 抽象
11. 子任务 / 捆绑点
12. problem-type-traditional
13. contest-core
14. contest-rule-acm
15. scoreboard-acm
16. module-registry
17. feature-flag-core
18. launcher / 模块安装器
```

短期最建议先做：

```text
统一错误响应
Problem Core
Dataset Core
problem-api 接入 Permission Core
```

不建议立刻做：

```text
contest-core
module-registry
launcher
```

原因是这些依赖：

```text
稳定题目模型
稳定数据模型
稳定评测模型
稳定权限模型
稳定路由注册方式
```

---

## 十三、文档与 README 的关系

`README.md` 应该保持：

```text
项目定位
当前状态
快速启动
核心模块
验收命令
下一阶段计划
```

`docs/*.md` 应该负责：

```text
详细设计
实现说明
排错命令
模块边界
后续规划
```

不要把所有内容都塞进 README。

README 太长会导致：

```text
入口阅读困难
细节难以维护
模块变更时容易漏改
```

因此 README 更像：

```text
项目地图
```

`docs/` 更像：

```text
模块说明书
```

---

## 十四、当前文档结论

当前 OJOS 文档体系应该围绕已经完成的真实状态展开：

```text
Permission Core 已完成
Judge Queue 已迁移 Redis Streams
NATS 已移除
Gateway 已负责统一入口和 JWT
Auth 已负责登录注册和 JWT
Judge API 已负责提交和任务投递
Judge Worker 已负责可靠消费和基础判题
```

后续所有新文档都应避免重复旧架构错误。

尤其不要再把当前架构写成：

```text
NATS 事件驱动
shared/events
NATS_URL
judge-worker subscribe submission.created
```

当前正确说法是：

```text
Redis Streams Consumer Group
ojos:judge:submissions
judge-workers
XREADGROUP
XACK
PostgreSQL PENDING fallback
```

这个文档索引的作用就是防止文档再次发散和互相矛盾。
