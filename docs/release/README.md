# 发布文档

本目录保存实现/测试入口和历史版本记录。生产放行结论只在上一级的 `production-readiness.md` 与
`release-candidate.md` 维护，避免把测试索引、历史记录和当前门禁混成一份文档。

- [evidence.md](evidence.md)：代码入口、测试名与证据边界。
- [candidate-promotion.md](candidate-promotion.md)：签名候选、11/22 文件集合、证据边界与只读晋级政策。
- [refactor-2026-07.md](refactor-2026-07.md)：2026-07 Web 控制面重构的历史记录；其中旧架构和缺口不代表 v1 当前状态。
- [../production-readiness.md](../production-readiness.md)：远端 workflow、密钥策略和生产缺口。
- [../release-candidate.md](../release-candidate.md)：当前候选判定。

发布时必须把证据绑定到精确 commit。历史 run、开发机输出和当前候选的 GitHub Actions artifact 应分别标注；
源码中的 `1.0.0` 版本号和 workflow 定义本身都不构成 GA 证据。

当前发布状态是 `SECURITY_ACCEPTANCE_PENDING`、`published=false`。候选 workflow 不由 tag
触发；未完成验收前不得运行既有候选晋级路径或创建 GitHub Release。
