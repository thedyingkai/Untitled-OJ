# Module Contract

> 文档状态：当前实现
> 最后更新：2026-06-27

## Contract Boundary

模块与 OJOS 主系统只通过以下 contract 交互：

- `module.yaml` manifest schema。
- `.ojosmod` package format。
- PostgreSQL module registry tables。
- Gateway Admin API。
- Frontend route/menu metadata。
- Operation history and audit log。

Installer core 不依赖 Go 代码，不依赖 frontend 代码。Gateway 也不直接读写本地模块文件，而是调用 internal Rust service。

## Declared Capabilities

一个模块可以声明：

- permissions
- components
- frontend_routes
- menus
- gateway_routes
- storage buckets
- health_checks
- migrations
- dependencies

未声明的能力不得由 installer 隐式公开。

## Security Contract

- 模块不能携带生产 secret。
- 模块不能绕过 Gateway、JWT、system.admin 权限和 internal auth。
- 模块包不能包含 `.env`、`.tmp`、`node_modules`、`frontend/dist`、`.git`、`target`。
- v0 不执行模块中的脚本或 hook。
- v0 不支持远程模块市场。

## Repository Boundary

当前 installer 放在 monorepo 内，但按可拆仓标准实现。拆仓触发条件见 ADR：

```text
docs/architecture/adr/ADR-module-installer-repository-boundary.md
```
