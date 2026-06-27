# Judge E2E 用例

> 文档状态：WSL2 Linux 环境已验收
> 适用范围：Judge Worker / E2E 验收 / 质量保证
> 最后更新：2026-06-27

## 1. 文档目的

本文档列出每种语言必须执行的评测用例，保证状态判定、资源限制和 sandbox 行为可验收。

## 2. 适用范围

适用于 `cpp17`、`c11`、`python3`、`java17` 的运行矩阵验证。

## 3. 当前实现

`services/judge-worker/config/languages.yaml` 定义语言配置，`scripts/e2e-linux.sh` 提供运行验收入口。

## 4. 目标设计

每种语言都应覆盖用户错误、系统限制和安全边界，避免某种语言绕过内存、时间或输出限制。

## 5. 用例矩阵

| 用例 | 预期状态 |
| --- | --- |
| 正确 A+B | `ACCEPTED` |
| 输出错误 | `WRONG_ANSWER` |
| 语法错误 | `COMPILE_ERROR` |
| 崩溃或异常 | `RUNTIME_ERROR` |
| 无限循环 | `TIME_LIMIT_EXCEEDED` |
| 大内存分配 | `MEMORY_LIMIT_EXCEEDED` |
| 无限 stdout/stderr | `OUTPUT_LIMIT_EXCEEDED` |
| 非法文件读取 | sandbox 拒绝 |
| fork/process bomb | 被限制，不影响宿主机 |

## 6. 配置说明

用例依赖 Linux worker、nsjail、cgroup v2、语言工具链和有效 worker token。

## 7. 安全边界

测试代码不得拖垮宿主机；fork bomb、非法文件读取和大输出必须被 sandbox 限制。

## 8. 验收方式

```bash
OJOS_WORKER_TOKEN=<token> bash scripts/e2e-linux.sh
```

脚本退出 0 才能记录通过。

## 9. 2026-06-27 验收结果

已在 `Ubuntu-24.04-OJOS` WSL2 Linux 环境执行状态矩阵：

| 语言 | AC | WA | CE | RE | TLE | MLE | OLE |
| --- | --- | --- | --- | --- | --- | --- | --- |
| `cpp17` | 通过 | 通过 | 通过 | 通过 | 通过 | 通过 | 通过 |
| `c11` | 通过 | 通过 | 通过 | 通过 | 通过 | 通过 | 通过 |
| `python3` | 通过 | 通过 | 通过 | 通过 | 通过 | 通过 | 通过 |
| `java17` | 通过 | 通过 | 通过 | 通过 | 通过 | 通过 | 通过 |

总计 28 个状态用例，失败数 0。`memory_kb` 非 0 记录 24 条，不存在长期恒为 0 的问题。fork bomb 防护、TLE 后进程清理、路径泄露扫描和权限复扫均通过。

## 10. 常见问题

- MLE 不稳定：检查 cgroup memory controller。
- OLE 不触发：检查输出文件限制。
- Java/Python 差异：检查语言独立配置。

## 11. 相关文档

- [资源限制](judge-resource-limits.md)
- [语言运行时](judge-language-runtime.md)
