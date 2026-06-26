# 临时文件隔离规则

> 文档状态：当前实现
> 适用范围：开发 / 文档维护 / 仓库清理 / 自动化代理
> 最后更新：2026-06-26

## 1. 文档目的

本文档规定 OJOS 仓库内临时文件、扫描报告、中间结果、调试日志和一次性脚本的唯一允许位置。目标是避免根目录、`docs/`、`services/`、`frontend/`、`deploy/`、`scripts/` 等正式目录被临时产物污染，保证仓库长期可维护、可审计、可交付。

## 2. 适用范围

本文档适用于所有开发者、自动化脚本和 AI 代理。凡是不准备作为正式源代码、正式配置、正式文档或正式部署脚本提交的文件，都必须视为临时文件。

## 3. 唯一允许目录

临时文件只能写入 `.tmp/agent/` 及其子目录：

| 目录 | 用途 |
| --- | --- |
| `.tmp/agent/reports/` | 扫描报告、清理清单、验证输出摘要 |
| `.tmp/agent/scripts/` | 一次性辅助脚本、临时分析脚本 |
| `.tmp/agent/patches/` | 临时 patch、diff 草稿 |
| `.tmp/agent/logs/` | 调试日志、命令输出日志 |
| `.tmp/agent/scratch/` | 临时草稿、数据整理中间文件 |
| `.tmp/agent/quarantine/` | 从正式目录隔离出的可疑文件或历史残留 |

## 4. 禁止写入的位置

以下目录禁止存放临时文件：

- 项目根目录。
- `docs/`，除非是正式文档。
- `services/`，除非是正式代码或测试。
- `frontend/`，除非是正式前端源码、类型或资源。
- `deploy/`，除非是正式部署配置或迁移。
- `scripts/`，除非脚本有长期维护价值、文件头说明和文档引用。
- `storage/`，除非是运行环境真实 artifact，不应纳入 Git。

## 5. 当前实现

仓库 `.gitignore` 必须忽略：

```gitignore
.tmp/
*.tmp
*.bak
*.old
*.orig
*.log
.DS_Store
Thumbs.db
```

当前清理流程使用 `.tmp/agent/reports/` 存放扫描结果，例如 `root-files.txt`、`all-files.txt`、`docs-english-scan.txt` 和 `docs-danger-scan.txt`。隔离出的根目录历史压缩包放入 `.tmp/agent/quarantine/`，不作为正式产物。

## 6. 归档与隔离规则

有参考价值的旧文档应归档到 `docs/archive/`，并加中文归档警告。无法确认用途但不应直接删除的文件，应移动到 `.tmp/agent/quarantine/`，在最终报告中列出名称、原路径、移动原因和后续处理建议。

一次性脚本不允许留在 `scripts/`。如果脚本需要长期保留，必须满足：

- 文件头说明用途。
- 说明运行环境。
- 说明从哪个目录执行。
- 列出依赖工具。
- 说明失败处理方式。
- 在正式文档中有入口链接或引用。

## 7. 安全边界

临时目录不能存放生产 secret、数据库 dump、用户提交源码、题目私有数据或未脱敏日志。如果必须临时分析敏感数据，应在本机隔离环境处理，不能提交到仓库。

## 8. 验收方式

执行以下命令生成清理清单：

```powershell
Get-ChildItem . -Force | Select-Object Mode, Length, LastWriteTime, Name
Get-ChildItem . -Recurse -Force -File | Where-Object { $_.FullName -notmatch "\\.tmp\\" }
```

预期结果：根目录不出现 `.tmp/agent/` 以外的临时报告、临时脚本、草稿或调试日志。`.gitignore` 中包含上述忽略规则。

## 9. 常见问题

### 是否可以把扫描报告放到 `docs/`？

不可以。扫描报告是过程产物，应放到 `.tmp/agent/reports/`。只有整理后的正式结论才写入 `docs/`。

### 是否可以在 `scripts/` 放临时 PowerShell？

不可以。临时脚本必须放到 `.tmp/agent/scripts/`。只有可长期维护、被文档引用的脚本才能进入 `scripts/`。

### 隔离文件是否需要删除？

不应在没有确认价值前盲删。先移动到 `.tmp/agent/quarantine/`，最终报告中列出，由维护者决定是否永久删除。

## 10. 相关文档

- [静态验证](static-verification.md)
- [编码规范](coding-standards.md)
- [文档索引](../DOCS_INDEX.md)
