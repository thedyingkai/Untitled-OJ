# 仓库文件隔离规则

> 文档状态：当前实现
> 适用范围：开发 / 文档维护 / 仓库清理 / 自动化代理
> 最后更新：2026-06-28

## 1. 文档目的

本文档规定 OJOS 仓库内本地产物、中间结果、调试日志和一次性脚本的处理规则。正式目录只能保存可维护源码、配置、文档、测试和部署入口。

## 2. 禁止进入 Git 的内容

- `.env` 和 `.env.*`，但 `.env.example` 除外。
- `.tmp/`、`tmp/`、`temp/`。
- `target/`、`frontend/dist/`、`frontend/node_modules/`、`node_modules/`。
- `*.ojosmod`、临时 plan JSON、release artifact。
- `*.log`、`compose-logs.txt`、`compose-ps.txt`。
- `tokens.local.json`、本地审计报告、截图、调试 dump。

## 3. 本地过程目录

运行过程中确实需要短期文件时，只能写入 `.tmp/agent/` 及其子目录，并且审计或提交前必须清理。

| 目录 | 允许用途 |
| --- | --- |
| `.tmp/agent/reports/` | 本地过程报告、命令输出摘要 |
| `.tmp/agent/scripts/` | 短期本地辅助脚本，完成后删除 |
| `.tmp/agent/patches/` | 临时 patch、diff 草稿 |
| `.tmp/agent/logs/` | 调试日志 |
| `.tmp/agent/scratch/` | 临时 package、plan JSON、数据整理中间文件 |
| `.tmp/agent/quarantine/` | 短期隔离文件，确认无价值后删除 |

`.tmp/agent/` 不是归档目录，也不是正式审计依据。

## 4. 正式脚本规则

一次性脚本不允许留在 `scripts/`。只有可长期维护、可重复执行、被文档引用并有明确失败处理方式的脚本才能进入 `scripts/`。

正式脚本必须满足：

- 文件头说明用途。
- 说明运行环境和执行目录。
- 列出依赖工具。
- 失败时返回非零退出码。
- 不打印 secret、token、password 或本机绝对路径。

## 5. 文档归档规则

- 当前真实能力写入正式文档目录。
- 历史过程写入 `docs/archive/`。
- 未来规划写入 `docs/features/` 或 `docs/roadmap/`。
- 已被正式文档覆盖、重复、编码损坏或无法作为当前依据的旧稿应删除。

## 6. 安全边界

本地过程目录不能存放生产 secret、数据库 dump、用户提交源码、题目私有数据或未脱敏日志。如果必须分析敏感数据，应在本机隔离环境处理，且不能提交到仓库。

## 7. 提交前确认

提交前至少确认：

```powershell
git status --short
git ls-files | Select-String -Pattern "\.env$|\.tmp|target|frontend/dist|node_modules|\.ojosmod|\.log|compose-logs|compose-ps|tokens.local"
```

预期：没有 tracked 垃圾文件；本地产物只出现在 ignored 状态中，并在审计结束后清理。

## 8. 相关文档

- [静态验证](static-verification.md)
- [编码规范](coding-standards.md)
- [文档索引](../docs-index.md)
