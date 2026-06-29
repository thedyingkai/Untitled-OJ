# Judge 语言运行时

> 文档状态：需要运行验收
> 适用范围：Judge Worker / 语言配置 / E2E 验收
> 最后更新：2026-06-26

## 1. 文档目的

本文档说明 OJOS judge-worker 支持语言的配置位置、运行方式和验收要求。

## 2. 适用范围

适用于维护 `services/judge-worker/config/languages.yaml`、前端语言选择、Judge API 语言列表和 E2E 用例的开发者。

## 3. 当前实现

当前至少支持 `cpp17`、`c11`、`python3`、`java17`。前端通过 `GET /api/judge/languages` 获取启用语言。

## 4. 目标设计

每种语言应有独立编译命令、运行命令、文件后缀、编译限制、运行限制和安全边界。禁用语言后端必须拒绝提交。

## 5. 关键流程

用户选择语言提交代码；Judge API 校验 language id；worker 按语言配置写源文件、编译、运行、收集状态。

## 6. 配置说明

语言配置位于 `services/judge-worker/config/languages.yaml`。题目包限制提供 time/memory，worker 配置提供输出和文件大小限制。

## 7. 安全边界

语言运行不能访问宿主机敏感路径。Java/Python 等运行时也必须受 cgroup 和 nsjail 限制。

## 8. 验收方式

每种语言执行 AC、WA、CE、RE、TLE、MLE、OLE、非法文件读取、fork 限制和大 stderr 限制。

## 9. 常见问题

- Java MLE 不准：检查 JVM 参数和 cgroup。
- Python TLE 不准：检查外层 wall timeout。
- C++ 编译失败：检查工具链和编译命令。

## 10. 相关文档

- [评测 E2E 用例](judge-e2e-cases.md)
- [资源限制](judge-resource-limits.md)
