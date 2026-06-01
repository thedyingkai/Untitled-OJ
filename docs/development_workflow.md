# OJOS 开发流程文档

## 一、文档定位

本文档记录 OJOS 当前阶段的本地开发流程、代码生成流程、版本管理规则、构建验证流程和常见问题排查方式。

它不是模块设计文档，而是实际开发时应该遵守的工作流文档。

适用范围：

```text
本地开发
GoLand 开发
go-zero 代码生成
Docker Compose 调试
Git 提交前检查
NATS 清理确认
Redis Streams Judge Queue 验收
Permission Core 验收
前端基础构建
```

当前 OJOS 是一个 Monorepo 项目，包含：

```text
Go 微服务
Rust Judge Worker
Vue / Vite 前端
Docker Compose
PostgreSQL migrations
文档
脚本
```

因此开发时不能只关注单个服务能不能跑，还需要保证：

```text
源码进入版本管理
生成文件进入版本管理
构建产物不进入版本管理
数据库迁移文件进入版本管理
Docker Compose 配置进入版本管理
NATS 残留不重新出现
Redis Streams 链路可用
所有核心服务能独立 build
```

---

## 二、当前项目结构

当前推荐根目录结构：

```text
Untitled-OJ/

├── frontend/
│   ├── public/
│   ├── src/
│   ├── package.json
│   ├── package-lock.json
│   ├── vite.config.ts
│   └── tsconfig*.json
│
├── services/
│   ├── shared/
│   ├── gateway/
│   ├── auth/
│   ├── judge-api/
│   └── judge-worker/
│
├── deploy/
│   ├── compose/
│   │   └── docker-compose.yml
│   └── migrations/
│       ├── 000001_init_schema.up.sql
│       ├── 000001_init_schema.down.sql
│       ├── 000002_judge_schema.up.sql
│       ├── 000002_judge_schema.down.sql
│       ├── 000003_permission_core.up.sql
│       └── 000003_permission_core.down.sql
│
├── docs/
│   ├── index.md
│   ├── architecture_overview.md
│   ├── shared_module.md
│   ├── auth_module.md
│   ├── gateway_module.md
│   ├── permission_core_module.md
│   ├── judge_module.md
│   ├── judge_worker_module.md
│   └── development_workflow.md
│
├── scripts/
│   └── gen-gozero.ps1
│
├── README.md
└── .gitignore
```

当前核心服务：

```text
shared
gateway
auth
judge-api
judge-worker
```

当前前端：

```text
frontend
```

当前基础设施：

```text
postgres
redis
jaeger
```

当前已移除：

```text
nats
```

---

## 三、开发环境要求

### 3.1 必备工具

本地开发建议安装：

```text
Git
Go
Rust
Cargo
Node.js
npm
Docker Desktop
PowerShell
GoLand
goctl
golang-migrate
```

当前项目使用：

```text
Go 微服务
Rust Worker
Vue / Vite 前端
Docker Compose
PostgreSQL migrations
Redis Streams
```

因此至少要保证以下命令可用：

```powershell
git --version
go version
rustc --version
cargo --version
node -v
npm -v
docker version
docker compose version
goctl -v
migrate -version
```

如果某个命令不可用，应先修本地环境，不要继续改业务代码。

---

### 3.2 Go 版本

当前服务使用 Go module 独立管理：

```text
services/shared/go.mod
services/auth/go.mod
services/gateway/go.mod
services/judge-api/go.mod
```

每个 Go 服务都是独立 module。

各服务通过：

```go
replace ojos-shared => ../shared
```

引用本地 Shared。

GoLand 可能提示：

```text
提交本地路径可能无法移植
```

当前可以接受。

原因：

```text
当前是 monorepo
shared 不作为独立远程 module 发布
Docker build context 也基于 monorepo
本地 replace 是当前最直接稳定的方案
```

---

### 3.3 Rust 版本

`judge-worker` 是 Rust 应用程序。

路径：

```text
services/judge-worker
```

需要能执行：

```powershell
cd D:\Untitled-OJ\services\judge-worker

cargo fmt
cargo build
```

`Cargo.lock` 必须提交。

因为 `judge-worker` 是应用程序，不是单纯 library，提交 lock 文件可以保证依赖版本一致。

---

### 3.4 Node / Frontend

前端位于：

```text
frontend
```

本地安装依赖：

```powershell
cd D:\Untitled-OJ\frontend

npm install
```

启动开发服务：

```powershell
npm run dev
```

构建：

```powershell
npm run build
```

`frontend/package-lock.json` 应提交。

`frontend/node_modules/` 不应提交。

---

## 四、Git 工作流

### 4.1 查看状态

每次开始修改前，先执行：

```powershell
cd D:\Untitled-OJ

git status -uall
```

需要关注：

```text
Changes not staged for commit
Untracked files
Deleted files
Ignored files
```

当前项目中，很多 go-zero 生成文件也是源码，不能因为是“生成的”就不提交。

---

### 4.2 查看未跟踪文件

```powershell
git ls-files --others --exclude-standard
```

如果看到以下内容，通常应该提交：

```text
deploy/migrations/*.sql
services/auth/**/*.go
services/gateway/**/*.go
services/judge-api/**/*.go
services/shared/**/*.go
services/judge-worker/src/**/*.rs
services/judge-worker/config/languages.yaml
services/judge-worker/Cargo.toml
services/judge-worker/Cargo.lock
frontend/package.json
frontend/package-lock.json
frontend/src/**
frontend/public/**
docs/**
README.md
scripts/*.ps1
```

如果看到以下内容，通常不应该提交：

```text
frontend/node_modules/
services/judge-worker/target/
services/*/*.exe
*.log
.env
tmp/
dist/
build/
```

---

### 4.3 查看被忽略文件

```powershell
git status --ignored -uall
```

或：

```powershell
git ls-files --others --ignored --exclude-standard
```

如果重要源码被忽略，需要修 `.gitignore`。

应该被忽略：

```text
frontend/node_modules/
services/judge-worker/target/
services/auth/*.exe
services/gateway/*.exe
services/judge-api/*.exe
.env
*.log
```

不应该被忽略：

```text
deploy/migrations/*.sql
deploy/compose/docker-compose.yml
services/**/go.mod
services/**/go.sum
services/judge-worker/Cargo.toml
services/judge-worker/Cargo.lock
services/**/etc/*.yaml
frontend/package-lock.json
docs/**
README.md
```

---

### 4.4 检查某个文件是否被忽略

例如检查 node_modules：

```powershell
git check-ignore -v frontend/node_modules
```

应该有输出。

检查重要源码：

```powershell
git check-ignore -v deploy/migrations/000003_permission_core.up.sql
git check-ignore -v services/shared/security/permission/permission.go
git check-ignore -v services/judge-worker/Cargo.lock
git check-ignore -v frontend/package-lock.json
```

这些应该没有输出。

如果有输出，说明 `.gitignore` 误伤，需要修。

---

### 4.5 添加文件

当前项目建议按模块添加：

```powershell
git add .gitignore
git add README.md
git add docs
git add scripts

git add deploy/compose/docker-compose.yml
git add deploy/migrations

git add services/shared
git add services/auth
git add services/gateway
git add services/judge-api
git add services/judge-worker

git add frontend
```

如果只想添加某个模块：

```powershell
git add services/auth
```

如果需要记录删除文件，也应使用：

```powershell
git add -A
```

因为 `git add -A` 会记录：

```text
新增文件
修改文件
删除文件
```

这在删除 NATS 文件时很重要，例如：

```text
services/shared/events/event.go
services/shared/events/nats.go
services/judge-worker/src/event.rs
```

这些删除也必须进入提交。

---

### 4.6 提交前检查 staged 文件

执行：

```powershell
git status -uall
```

确认 staged 中不包含：

```text
node_modules/
target/
*.exe
services/judge-api/-Method
根目录 package-lock.json（如果根目录没有 package.json）
```

如果 `.exe` 已经被跟踪，`.gitignore` 不会自动移除，需要：

```powershell
git ls-files "*.exe"
```

如果有输出：

```powershell
git ls-files "*.exe" | ForEach-Object { git rm --cached $_ }
```

如果误生成了：

```text
services/judge-api/-Method
```

删除：

```powershell
Remove-Item .\services\judge-api\-Method -Force
```

---

### 4.7 提交

确认构建和验收通过后再提交：

```powershell
git commit -m "feat: add permission core and migrate judge queue to redis streams"
```

如果本地已经 ahead：

```text
Your branch is ahead of 'origin/main' by 1 commit
```

说明有提交尚未推送。

提交后统一推送：

```powershell
git push
```

---

## 五、.gitignore 规则

当前 `.gitignore` 应适配：

```text
Go
Rust
Vue / Node
Docker 本地数据
日志
临时文件
构建产物
```

应该忽略：

```gitignore
# OS
.DS_Store
Thumbs.db
desktop.ini

# IDE
.idea/
.vscode/
*.swp
*.swo
*~

# env
.env
.env.*
!.env.example

# logs
*.log
logs/
log/

# temp
tmp/
temp/
.cache/
*.tmp
*.bak
*.backup

# Go binaries
*.exe
*.exe~
*.dll
*.so
*.dylib
*.test
*.out
coverage.*
*.coverprofile
profile.cov

# Go workspace
go.work
go.work.sum

# vendor
vendor/

# Rust
target/
services/*/target/

# Node
node_modules/
frontend/node_modules/
dist/
build/
.next/
.nuxt/
coverage/

npm-debug.log*
yarn-debug.log*
yarn-error.log*
pnpm-debug.log*

# Python cache
__pycache__/
*.py[cod]
.pytest_cache/
.venv/
venv/

# local docker/runtime data
docker-data/
data/
.local-data/

# dumps
*.dump
*.sql.gz
```

注意：

```text
不要忽略 Cargo.lock
不要忽略 package-lock.json
不要忽略 deploy/migrations/*.sql
不要忽略 services/**/etc/*.yaml
不要忽略 go.mod / go.sum
不要忽略 docs/**
```

如果项目中确实要提交 `.pdf / .docx / .pptx / .xlsx`，不要把这些全局忽略。当前如果只是源码项目，可以忽略导出文档，但要按项目实际需要决定。

---

## 六、go-zero 代码生成流程

当前 Go 服务中，以下模块使用 go-zero：

```text
auth
gateway
judge-api
```

对应 `.api` 文件：

```text
services/auth/auth.api
services/gateway/gateway.api
services/judge-api/judgeapi.api
```

---

### 6.1 单服务生成

Auth：

```powershell
cd D:\Untitled-OJ\services\auth

goctl api go -api auth.api -dir . --style gozero
```

Gateway：

```powershell
cd D:\Untitled-OJ\services\gateway

goctl api go -api gateway.api -dir . --style gozero
```

Judge API：

```powershell
cd D:\Untitled-OJ\services\judge-api

goctl api go -api judgeapi.api -dir . --style gozero
```

---

### 6.2 使用统一脚本

推荐脚本：

```text
scripts/gen-gozero.ps1
```

示例：

```powershell
cd D:\Untitled-OJ

.\scripts\gen-gozero.ps1
```

只生成某个服务：

```powershell
.\scripts\gen-gozero.ps1 -Service auth
.\scripts\gen-gozero.ps1 -Service gateway
.\scripts\gen-gozero.ps1 -Service judge-api
```

根据变更文件生成：

```powershell
.\scripts\gen-gozero.ps1 -ChangedFile "D:\Untitled-OJ\services\auth\auth.api"
```

---

### 6.3 脚本建议内容

`scripts/gen-gozero.ps1` 推荐内容：

```powershell
param(
    [string]$Service = "",
    [string]$ChangedFile = ""
)

$ErrorActionPreference = "Stop"

$Root = Split-Path -Parent $PSScriptRoot
$ServicesRoot = Join-Path $Root "services"

function Invoke-GoctlGenerate {
    param(
        [string]$ServiceName
    )

    $ServiceDir = Join-Path $ServicesRoot $ServiceName

    if (!(Test-Path $ServiceDir)) {
        throw "service dir not found: $ServiceDir"
    }

    $ApiFiles = Get-ChildItem $ServiceDir -Filter "*.api" -File

    if ($ApiFiles.Count -eq 0) {
        Write-Host "no api file found in $ServiceDir"
        return
    }

    foreach ($ApiFile in $ApiFiles) {
        Push-Location $ServiceDir
        try {
            Write-Host "generating service: $ServiceName, api: $($ApiFile.Name)"
            goctl api go -api $ApiFile.Name -dir . --style gozero

            $GoFiles = Get-ChildItem . -Recurse -Filter "*.go" -File
            if ($GoFiles.Count -gt 0) {
                gofmt -w ($GoFiles | ForEach-Object { $_.FullName })
            }
        }
        finally {
            Pop-Location
        }
    }
}

if ($ChangedFile -ne "") {
    $Full = [System.IO.Path]::GetFullPath($ChangedFile)

    if ($Full -match "\\services\\([^\\]+)\\.*\.api$") {
        Invoke-GoctlGenerate -ServiceName $Matches[1]
        exit 0
    }

    Write-Host "changed file is not a service api file: $ChangedFile"
    exit 0
}

if ($Service -ne "") {
    Invoke-GoctlGenerate -ServiceName $Service
    exit 0
}

$DefaultServices = @(
    "auth",
    "gateway",
    "judge-api"
)

foreach ($Name in $DefaultServices) {
    Invoke-GoctlGenerate -ServiceName $Name
}
```

---

### 6.4 go-zero 生成文件是否提交

必须提交。

原因：

```text
handler / logic / types / routes 是 Go 服务源码的一部分
Docker build 不应该依赖运行时重新生成
CI 构建时不应该因为缺 goctl 导致失败
其他开发者 clone 后应直接 go build
```

应该提交：

```text
services/auth/internal/handler
services/auth/internal/logic
services/auth/internal/types
services/auth/internal/handler/routes.go

services/gateway/internal/handler
services/gateway/internal/logic
services/gateway/internal/types
services/gateway/internal/handler/routes.go

services/judge-api/internal/handler
services/judge-api/internal/logic
services/judge-api/internal/types
services/judge-api/internal/handler/routes.go
```

不要把这些文件当成临时文件。

---

### 6.5 避免生成文件覆盖业务逻辑

go-zero 重新生成可能覆盖部分生成文件。

为了降低风险：

```text
业务逻辑尽量放 service 层或 repository 层
logic 层尽量保持薄
每次生成前先 git diff
生成后检查 git diff
```

推荐职责：

```text
handler: 只负责 HTTP glue
logic: 只负责参数转 service
service: 核心业务
repository: 数据库访问
```

这样即使 logic 被重新生成，核心业务也不容易丢。

---

## 七、GoLand 自动生成配置

### 7.1 External Tool

GoLand 中可以配置手动生成工具。

路径：

```text
Settings / Preferences
  -> Tools
  -> External Tools
  -> +
```

配置：

```text
Name:
Generate go-zero services

Program:
powershell.exe

Arguments:
-ExecutionPolicy Bypass -File "$ProjectFileDir$\scripts\gen-gozero.ps1"

Working directory:
$ProjectFileDir$
```

建议勾选：

```text
Synchronize files after execution
Open console for tool output
```

使用：

```text
Tools -> External Tools -> Generate go-zero services
```

---

### 7.2 File Watcher

可以配置 `.api` 文件保存后自动生成。

路径：

```text
Settings / Preferences
  -> Tools
  -> File Watchers
  -> +
  -> Custom
```

配置：

```text
Name:
goctl api generate

Program:
powershell.exe

Arguments:
-ExecutionPolicy Bypass -File "$ProjectFileDir$\scripts\gen-gozero.ps1" -ChangedFile "$FilePath$"

Working directory:
$ProjectFileDir$
```

监听范围只应该包括：

```text
services/*/*.api
```

不要监听：

```text
*.go
```

否则会出现循环：

```text
保存 .api
    -> goctl 生成 .go
    -> .go 变化触发 watcher
    -> 再生成
```

---

### 7.3 GoLand 前端警告处理

常见警告：

```text
frontend/index.html 无法解析 /src/main.ts
```

这是因为 Vite 的 `/src/main.ts` 是以前端根目录为基准，GoLand 在 monorepo 下可能误判。

只要下面命令通过，可以先忽略：

```powershell
cd D:\Untitled-OJ\frontend

npm run dev
npm run build
```

常见警告：

```text
vue.svg 命名空间未使用
XML 标签空体
```

这是 IDE 对 SVG 的静态检查，不影响构建。

常见警告：

```text
font-family 没有通用默认值
```

建议修 CSS：

```css
font-family: Inter, system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
```

代码字体：

```css
font-family: "JetBrains Mono", "Fira Code", Consolas, monospace;
```

---

## 八、数据库迁移流程

当前 migration 目录：

```text
deploy/migrations
```

当前已有迁移：

```text
000001_init_schema.up.sql
000001_init_schema.down.sql
000002_judge_schema.up.sql
000002_judge_schema.down.sql
000003_permission_core.up.sql
000003_permission_core.down.sql
```

---

### 8.1 执行迁移

宿主机执行：

```powershell
cd D:\Untitled-OJ

migrate `
  -path deploy/migrations `
  -database "postgres://postgres:password@localhost:5433/ojos?sslmode=disable" `
  up
```

注意：

```text
宿主机连接 PostgreSQL 用 localhost:5433
容器内服务连接 PostgreSQL 用 postgres:5432
```

---

### 8.2 回滚

回滚一步：

```powershell
migrate `
  -path deploy/migrations `
  -database "postgres://postgres:password@localhost:5433/ojos?sslmode=disable" `
  down 1
```

开发环境可以用，生产谨慎。

---

### 8.3 新建迁移

```powershell
migrate create `
  -ext sql `
  -dir deploy/migrations `
  -seq problem_core
```

会生成：

```text
000004_problem_core.up.sql
000004_problem_core.down.sql
```

---

### 8.4 migration 规则

规则：

```text
已提交的 migration 不要随便改历史
新增结构用新 migration
down 文件必须谨慎
基础表数据初始化要可重复执行
使用 ON CONFLICT DO NOTHING
权限点注册要可重复执行
资源类型注册要可重复执行
```

如果只是本地还没推送的迁移，可以改；如果已经被其他人使用，不能随意修改历史。

---

## 九、Docker Compose 工作流

Compose 目录：

```text
deploy/compose
```

启动全部：

```powershell
cd D:\Untitled-OJ\deploy\compose

docker compose up -d --build
```

重建单服务：

```powershell
docker compose up -d --build gateway
docker compose up -d --build auth
docker compose up -d --build judge-api
docker compose up -d --build judge-worker
```

关闭并清 orphan：

```powershell
docker compose down --remove-orphans
```

查看容器：

```powershell
docker ps
```

查看日志：

```powershell
docker logs ojos-gateway --tail 100
docker logs ojos-auth --tail 100
docker logs ojos-judge-api --tail 100
docker logs ojos-judge-worker --tail 100
docker logs ojos-postgres --tail 100
docker logs ojos-redis --tail 100
```

---

## 十、NATS 清理检查

当前 OJOS 已经从 NATS 迁移到 Redis Streams。

因此代码、配置、Compose、依赖中不应再出现 NATS。

全项目检查：

```powershell
cd D:\Untitled-OJ

Get-ChildItem .\services,.\deploy -Recurse -Include *.go,*.rs,*.toml,*.yaml,*.yml,go.mod,go.sum,Cargo.toml |
  Select-String -Pattern "nats|NATS|Nats|async_nats|async-nats|4222"
```

预期：

```text
无输出
```

检查 compose：

```powershell
cd D:\Untitled-OJ\deploy\compose

docker compose config | Select-String -Pattern "nats|4222"
```

预期：

```text
无输出
```

检查容器：

```powershell
docker ps --filter "name=nats"
```

预期：

```text
无输出
```

注意：

```text
event-listener 不是 NATS
zipkin 不是 NATS
```

不要误删。

---

## 十一、Redis Streams 验收

当前 Judge 队列使用：

```text
Stream: ojos:judge:submissions
Group:  judge-workers
```

查看 Stream：

```powershell
docker exec -it ojos-redis redis-cli XINFO STREAM ojos:judge:submissions
```

查看 Group：

```powershell
docker exec -it ojos-redis redis-cli XINFO GROUPS ojos:judge:submissions
```

查看 Pending：

```powershell
docker exec -it ojos-redis redis-cli XPENDING ojos:judge:submissions judge-workers
```

查看历史：

```powershell
docker exec -it ojos-redis redis-cli XRANGE ojos:judge:submissions - +
```

正常状态：

```text
XPENDING = 0
```

说明 worker 消费后已经 XACK。

如果 `XRANGE` 仍看到历史消息，这是正常的。

`XACK` 不删除历史消息，只确认 consumer group 的 pending。

---

## 十二、Permission Core 验收

### 12.1 查看用户角色

```powershell
docker exec -it ojos-postgres psql -U postgres -d ojos
```

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

### 12.2 写入 deny

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

此时提交应返回 forbidden。

---

### 12.3 删除 deny

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

## 十三、完整功能验收流程

### 13.1 启动服务

```powershell
cd D:\Untitled-OJ\deploy\compose

docker compose up -d --build
```

### 13.2 Gateway Health

```powershell
Invoke-RestMethod `
  -Method Get `
  -Uri "http://localhost:8080/health"
```

预期：

```text
status = ok
```

### 13.3 登录

```powershell
$body = @{
  username = "permtest"
  password = "123456"
} | ConvertTo-Json -Compress

$res = Invoke-RestMethod `
  -Method Post `
  -Uri "http://localhost:8080/api/auth/login" `
  -ContentType "application/json" `
  -Body $body

$token = $res.data.token
```

如果响应直接是 token：

```powershell
$token = $res.token
```

### 13.4 Profile

```powershell
Invoke-RestMethod `
  -Method Get `
  -Uri "http://localhost:8080/api/auth/profile" `
  -Headers @{ Authorization = "Bearer $token" }
```

预期包含：

```text
user_id
username
email
roles
```

### 13.5 提交代码

```powershell
$code = @'
#include <bits/stdc++.h>
using namespace std;

int main() {
    int a, b;
    cin >> a >> b;
    cout << a + b << endl;
    return 0;
}
'@

$body = @{
  problem_id = 1
  language = "cpp17"
  code = $code
} | ConvertTo-Json -Compress

$res = Invoke-RestMethod `
  -Method Post `
  -Uri "http://localhost:8080/api/judge/submissions" `
  -ContentType "application/json" `
  -Headers @{ Authorization = "Bearer $token" } `
  -Body $body

$res
```

预期：

```text
submission_id = 新 ID
status = PENDING
```

### 13.6 查 worker 日志

```powershell
docker logs ojos-judge-worker --tail 100
```

预期：

```text
received judge stream message
submission claimed
start judging
judge finished
judge stream message acked
```

### 13.7 查提交结果

```powershell
Start-Sleep -Seconds 2

Invoke-RestMethod `
  -Method Get `
  -Uri "http://localhost:8080/api/judge/submissions/$($res.submission_id)" `
  -Headers @{ Authorization = "Bearer $token" }
```

预期：

```text
status = ACCEPTED
score = 100
```

### 13.8 查 Redis Pending

```powershell
docker exec -it ojos-redis redis-cli XPENDING ojos:judge:submissions judge-workers
```

预期：

```text
0
```

---

## 十四、本地编译检查

### 14.1 shared

```powershell
cd D:\Untitled-OJ\services\shared

go mod tidy
go build ./...
```

### 14.2 auth

```powershell
cd D:\Untitled-OJ\services\auth

go mod tidy
go build .
```

### 14.3 gateway

```powershell
cd D:\Untitled-OJ\services\gateway

go mod tidy
go build .
```

### 14.4 judge-api

```powershell
cd D:\Untitled-OJ\services\judge-api

go mod tidy
go build .
```

### 14.5 judge-worker

```powershell
cd D:\Untitled-OJ\services\judge-worker

cargo fmt
cargo build
```

### 14.6 frontend

```powershell
cd D:\Untitled-OJ\frontend

npm install
npm run build
```

---

## 十五、Go 依赖常见问题

### 15.1 missing go.sum entry for zipkin

错误：

```text
missing go.sum entry for module providing package go.opentelemetry.io/otel/exporters/zipkin
```

原因：

```text
go-zero/core/trace 间接依赖 zipkin exporter
```

解决：

```powershell
go get github.com/zeromicro/go-zero/core/trace@v1.10.2
go mod tidy
go build .
```

不要因为 GoLand 提示 zipkin deprecated 就手动删除 go.sum 中相关内容。

zipkin 不是 NATS。

---

### 15.2 replace ojos-shared => ../shared 警告

GoLand 可能提示：

```text
提交本地路径可能无法移植
```

当前可以接受。

不要删除 replace，否则服务可能找不到本地 shared。

---

### 15.3 找不到 ojos-shared/events

错误：

```text
package ojos-shared/events is not in std
```

原因：

```text
旧 NATS events 引用未删除
```

排查：

```powershell
cd D:\Untitled-OJ\services

Get-ChildItem -Recurse -Include *.go |
  Select-String -Pattern "ojos-shared/events|events.NewBus|NewBusByURL"
```

找到后删除相关 import、字段和初始化逻辑。

---

## 十六、Rust 常见问题

### 16.1 `self` parameter is only allowed in associated functions

原因：

```text
在普通函数中写了 &self
```

错误：

```rust
pub async fn list_pending_submission_ids(&self, limit: i64)
```

正确：

```rust
pub async fn list_pending_submission_ids(db: &PgPool, limit: i64)
```

---

### 16.2 Duration 未导入

错误：

```text
use of undeclared type Duration
```

解决：

```rust
use std::time::Duration;
```

---

### 16.3 Redis Value 类型错误

错误：

```text
expected Value, found &Value
```

解决：

```rust
let text: String = redis::from_redis_value(value.clone()).ok()?;
```

---

### 16.4 dead_code warning

例如：

```text
fields id and user_id are never read
```

当前可以接受。

这是 Rust warning，不影响 build。

后续可以通过实际使用字段或调整结构体消除。

---

## 十七、Docker 常见问题

### 17.1 container start 报 no such file

错误类似：

```text
exec: "./judge-api": stat ./judge-api: no such file or directory
```

原因：

```text
Dockerfile 编译出的二进制名字和 compose command 不一致
```

排查：

```text
Dockerfile 中 go build -o 的名字
docker-compose.yml 中 command 的名字
```

必须一致。

例如：

```Dockerfile
RUN go build -o judge-api .
CMD ["./judge-api", "-f", "etc/judgeapi.yaml"]
```

---

### 17.2 docker logs 找不到容器

如果执行：

```powershell
docker logs ojos-judge-api
```

返回：

```text
No such container
```

先查：

```powershell
docker ps --filter "name=judge"
docker ps -a --filter "name=judge"
```

确认容器名。

可能是 compose service 名、container_name 和你输入的不一致。

---

### 17.3 服务改了但容器没更新

执行：

```powershell
cd D:\Untitled-OJ\deploy\compose

docker compose up -d --build judge-api
```

必要时：

```powershell
docker compose down --remove-orphans
docker compose up -d --build
```

---

## 十八、前端常见问题

### 18.1 GoLand 无法解析 /src/main.ts

这是 Vite monorepo 识别问题，不一定是代码错。

检查：

```powershell
cd D:\Untitled-OJ\frontend

npm run dev
npm run build
```

能通过即可。

---

### 18.2 node_modules 不要提交

确认：

```powershell
git check-ignore -v frontend/node_modules
```

应该有输出。

如果误加入 Git：

```powershell
git rm -r --cached frontend/node_modules
```

---

### 18.3 package-lock.json 要提交

确认：

```powershell
git check-ignore -v frontend/package-lock.json
```

应该无输出。

提交：

```powershell
git add frontend/package-lock.json
```

---

## 十九、提交前最终检查清单

每次重要提交前执行。

### 19.1 Git 状态

```powershell
cd D:\Untitled-OJ

git status -uall
```

确认没有误提交：

```text
node_modules/
target/
*.exe
services/judge-api/-Method
.env
```

---

### 19.2 NATS 清理

```powershell
Get-ChildItem .\services,.\deploy -Recurse -Include *.go,*.rs,*.toml,*.yaml,*.yml,go.mod,go.sum,Cargo.toml |
  Select-String -Pattern "nats|NATS|Nats|async_nats|async-nats|4222"
```

预期无输出。

---

### 19.3 Go build

```powershell
cd D:\Untitled-OJ\services\shared
go build ./...

cd D:\Untitled-OJ\services\auth
go build .

cd D:\Untitled-OJ\services\gateway
go build .

cd D:\Untitled-OJ\services\judge-api
go build .
```

---

### 19.4 Rust build

```powershell
cd D:\Untitled-OJ\services\judge-worker

cargo fmt
cargo build
```

---

### 19.5 Frontend build

```powershell
cd D:\Untitled-OJ\frontend

npm run build
```

---

### 19.6 Docker build

```powershell
cd D:\Untitled-OJ\deploy\compose

docker compose up -d --build
```

---

### 19.7 关键功能验收

```text
Gateway /health ok
Auth login ok
Auth profile ok
Judge submit ok
Judge Worker consumed Redis Stream
Submission ACCEPTED
Redis XPENDING = 0
```

---

## 二十、推荐提交粒度

不要把所有事情永远堆在一个 commit。

推荐粒度：

```text
feat: add permission core schema
feat: integrate permission core into judge api
feat: migrate judge queue to redis streams
chore: remove nats dependencies
docs: update architecture and module docs
chore: update gitignore and generated files
```

如果已经在本地一次性做了很多，也可以先提交一个较大的 checkpoint，但后续要逐步拆分。

---

## 二十一、当前结论

OJOS 当前开发流程的核心要求是：

```text
生成文件要进版本管理
构建产物不要进版本管理
NATS 不应重新出现
Redis Streams 是当前 Judge Queue
PostgreSQL 是最终事实源
Permission Core 是资源级权限核心
Gateway 只负责认证入口
业务服务负责授权判断
Judge Worker 当前不安全，不能公网开放
```

每次开发新模块前，应先保证：

```text
当前 main 分支可 build
当前 docker compose 可启动
当前基础验收可通过
当前文档与实现一致
```

下一阶段如果要做 `problem-api / dataset-core`，也应遵守当前流程：

```text
先设计 migration
再设计 .api
再生成 go-zero 结构
再接入 shared / permission
再写 service / repository
再写验收命令
最后更新文档
```

不要先乱写业务代码再补架构，否则会继续反复重构。
