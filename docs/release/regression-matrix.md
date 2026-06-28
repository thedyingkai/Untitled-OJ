# Regression Matrix

| 范围 | 验证脚本或命令 | 当前要求 | 失败处理 | 是否阻塞下一阶段 |
| --- | --- | --- | --- | --- |
| Auth | `scripts/e2e-api.ps1`、`services/auth` Go tests | 必须通过 | 修复登录、JWT、角色、权限检查 | 是 |
| Problem API | `scripts/e2e-api.ps1`、`services/problem-api` Go tests | 必须通过 | 检查题库 CRUD、packagefs 和错误响应 | 是 |
| Judge API | `scripts/e2e-api.ps1`、`services/judge-api` Go tests | 必须通过 | 检查提交、Worker Link、队列和结果 | 是 |
| Judge Worker | `services/judge-worker` Rust tests | 必须通过 | 修复题目包加载、运行时或回传逻辑 | 是 |
| Installer Core/Service/CLI | Rust root tests、`verify-static`、CLI smoke | 必须通过 | 修复 manifest、package、plan、operation history | 是 |
| Native Installer TUI | Rust build、`ojos-installer-tui --version` | 必须通过 | 修复 TUI 构建、键盘视图或计划展示 | 是 |
| Module Runtime | Gateway Go tests、runtime snapshot e2e | 必须通过 | 修复 snapshot、route table、topology 派生 | 是 |
| Dynamic Proxy | Gateway proxy tests、e2e route checks | 必须通过 | 修复 trusted service、auth、header、reserved prefix | 是 |
| Controlled Apply | `ojosctl runtime apply-plan --dry-run`，显式 confirm smoke | dry-run 必须通过，confirm 按发布验收执行 | 修复 allowlist、lock、history、redaction | 涉及 apply 时阻塞 |
| Module SDK | `ojosctl module init/package/verify` | 必须通过 | 修复 scaffold、schema、package format | 是 |
| Sample Module | `scripts/e2e-module-compat.ps1` | 必须通过 | 修复 sample manifest 或 registry/runtime 流 | 是 |
| Frontend Shell | `npm run build`、贡献视图 e2e | 必须通过 | 修复构建或 Runtime Snapshot 渲染 | 是 |
| Security Scan | `verify-static`、secret/path scan | 必须通过 | 删除 secret、路径泄露或危险配置 | 是 |
| Permission Rejection | e2e user/no-token checks | 普通用户 `403`，无 token `401` | 修复 middleware 或 admin boundary | 是 |
| Docs | `DOCS_INDEX`、`DOCS_STATUS`、release docs | 当前状态准确 | 修正旧阶段、英文流水账或过度承诺 | 是 |

任何失败都不能用“已知问题”绕过发布，除非该问题已经写入 `v0.1.0-known-limitations.md` 且不影响 v0.1.0 声明的可发布能力。
