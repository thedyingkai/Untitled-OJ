# OJOS

OJOS 是一个面向工程化部署的分布式 Online Judge 项目。它不是单体 demo，而是按 Control Plane、内部 API、外部 Judge Worker、前端管理台、PostgreSQL、Redis signal stream 和 artifact storage 拆分的 OJ 平台。

当前阶段目标是把 A/Core Judge 能力打磨成可部署、可维护、可观测、可扩展的基础平台，并为后续模块化安装器和 B/Contest 模块做准备。

## 当前状态

当前仓库已经实现或补齐的核心能力包括：

- Gateway 作为唯一公开入口，内部服务不直接暴露给普通用户。
- Auth 支持注册、登录、当前用户、角色、权限和资源级授权管理。
- Problem API 支持题目列表、详情、CRUD、题目包校验和题目包相关接口。
- Judge API 支持提交、提交列表、提交详情、case 结果、debug 日志、队列管理和 worker 管理。
- Judge Worker 使用 Rust 实现，通过 Worker Link 与 Control Plane 通信。
- Worker 不直连 PostgreSQL/Redis，也不挂载 Control Plane 的本地 storage。
- PostgreSQL 作为任务和业务事实源，Redis Streams 作为有界 signal history。
- 前端使用 Vue 3 + Vite + TypeScript + Naive UI，已接入真实 API。
- Module Registry v0 已登记 Kernel 内置模块和 `ojos.judge-core`，并提供后台模块拓扑页面。
- 中文工程文档体系已整理到 `docs/`。

仍需运行环境验收的内容：

- Docker daemon 下的完整 compose build/up。
- Linux cgroup v2 + nsjail 下的真实资源限制验收。
- 多台独立 worker 机器的并发评测。
- cpp17、c11、python3、java17 的 AC/WA/CE/RE/TLE/MLE/OLE 全矩阵运行。
- worker 崩溃恢复、旧 lease result 拒绝和多 worker 幂等评测的真实运行记录。

## 架构概览

```text
Browser
  |
  v
Gateway
  |-- Auth
  |-- Problem API
  |-- Judge API
        |
        v
   Worker Link API
        |
        v
 Judge Worker(s)

PostgreSQL: 业务数据、权限、提交、任务 lease、模块注册表
Redis: judge signal stream
Storage: problem package、submission artifact、judge artifact
```

长期目标是演进为 Kernel + Core + 可安装模块的 OJOS 平台：

- Kernel：身份、权限、配置、审计、模块运行时、前端 shell。
- Core Capability：题目、提交、评测、Worker Link、结果查询。
- Module Registry v0：当前只读登记和拓扑展示。
- Installer v0：下一阶段目标，提供 `validate / install / enable / disable`。
- B/Contest：后续第一个热插拔验证模块，当前尚未开始主体开发。

## 快速验证

本机静态验证：

```powershell
powershell -NoProfile -File scripts\verify-static.ps1 -SkipDockerBuild
```

该命令会执行 Go/Rust/Frontend 构建测试、compose config 和安全扫描。使用 `-SkipDockerBuild` 时不会执行 Docker 镜像构建。

Rust worker 单独验证：

```powershell
cd services\judge-worker
cargo fmt --check
cargo check
cargo test
```

前端构建：

```powershell
cd frontend
npm run build
```

Docker 运行验收需要先启动 Docker daemon，并准备 `.env.example` 中要求的 secret。Docker 未运行时，不应把 compose build/up、真实 HTTP API curl 或多 worker 验收记录为通过。

## 文档入口

- [文档中心](docs/README.md)
- [当前状态](docs/development/current-state.md)
- [架构总览](docs/architecture/overview.md)
- [服务拓扑](docs/architecture/service-topology.md)
- [模块拓扑设计](docs/architecture/module-topology.md)
- [Admin API](docs/api/admin-api.md)
- [Worker Link 协议](docs/architecture/worker-link-protocol.md)
- [Judge 资源限制](docs/judge/judge-resource-limits.md)
- [Control Plane 部署](docs/deploy/deploy-control-plane.md)
- [Worker Node 部署](docs/deploy/deploy-worker-node.md)
- [端到端工程验收](docs/e2e/e2e-engineering-acceptance.md)

## GitHub Wiki 托管建议

可以把文档托管到 GitHub Wiki，适合给团队或外部读者提供更友好的阅读入口。但建议保持以下规则：

- `docs/` 仍作为文档权威源，随代码一起 review、测试和版本化。
- GitHub Wiki 作为发布副本或阅读镜像，不直接承载唯一真相。
- Wiki 首页可以同步 `docs/README.md`，分类页可以同步 `docs/architecture`、`docs/deploy`、`docs/api`、`docs/judge`、`docs/security` 和 `docs/e2e`。
- 运行验收状态必须写清楚，不能在 Wiki 中把 Docker/nsjail/cgroup/多机 worker 未执行项标为已通过。

当前已经提供 GitHub Actions 自动同步流程：`.github/workflows/sync-wiki.yml` 会在 `main` 分支的 `docs/**`、根目录 `README.md` 或 workflow 自身变更后，把 `docs/` 镜像到 `Untitled-OJ.wiki.git`。也可以在 GitHub Actions 页面手动触发 `Sync Docs To Wiki`。

默认同步使用 `GITHUB_TOKEN`。如果仓库权限策略导致 Wiki 推送失败，可以在仓库 Secrets 中配置具备仓库写权限的 `WIKI_PUSH_TOKEN`，workflow 会优先使用它。

## 代码与文档约束

- Public API 不允许返回内部路径字段。
- Worker API 必须鉴权，worker token 不写入生产默认值。
- Gateway 到内部服务使用内部 HMAC 边界。
- problem-api、judge-api、PostgreSQL、Redis 不应对公网暴露。
- 临时文件只允许放在 `.tmp/agent/`，不要污染根目录和正式源码目录。

## 下一阶段

下一阶段建议只做：

```text
Installer v0：validate / install / enable / disable
```

不要直接开始 B/Contest 主体开发。Contest 应作为 installer v0 具备之后的第一个热插拔验证模块。
