# 发布文档

本目录保存可核对的实现证据和版本变更记录。生产放行结论在上一级目录维护，避免把测试索引、历史记录和当前门禁混成一份文档。

- [evidence.md](evidence.md)：代码入口、测试名与证据边界。
- [refactor-2026-07.md](refactor-2026-07.md)：2026-07 Web 控制面重构的改动和未完成项。
- [../production-readiness.md](../production-readiness.md)：远端 workflow、密钥策略和生产缺口。
- [../release-candidate.md](../release-candidate.md)：当前候选判定。

发布时必须把证据绑定到精确 commit。历史 run、开发机输出和当前候选的 GitHub Actions artifact 应分别标注。
