# 构建与交付

仓库只包含 OJOS 产品与交付所需的资产。所有软件测试、冒烟检查、故障演练、容量驱动、验证夹具和验收报告均在独立验证目录中维护，不属于 GitHub workflow 的职责。

## 仓库内的流水线

| 流水线 | 触发 | 输出 |
| --- | --- | --- |
| Product build | `main` 推送、PR、手动 | 编译 Go 服务和 SDK、Rust 控制面和 Worker、Web 产品。 |
| Orchestrator native portable | 相关 `main` 推送、手动 | Windows/Linux unsigned portable 包及 SHA-256。 |
| Orchestrator product images | 在 `main` 手动运行 | 以源码 SHA 标记的控制面和 Agent OCI 镜像、构建 provenance 与 SBOM。 |
| Sync Docs To Wiki | 文档更新、手动 | 产品文档镜像。 |

原生打包保留文件布局、平台和内容摘要校验；这些是安装器的产品完整性职责，不是测试场景。流水线不安装测试框架，不启动测试服务器，不运行单元测试、e2e 或演练。

## 外部验证与正式发布

本机独立验证目录为 `D:\Untitled-OJ-external-tests`。原始测试和发布验收工具、迁移清单、历史基线归档都保存在那里。外部验证必须复制当前产品源码并记录具体提交和工作树状态，不能只运行旧基线。

此前与容量/e2e 绑定的 signed-GA 候选和晋级流程已外移。安全验收仍未因此获得批准；本仓库不自动创建正式 GitHub Release，也不把 unsigned portable 当成已签名 GA。需要正式发布时，在外部验收环境完成既有的源码身份、签名、证明和安全验收，再由发布负责人决定晋级。不得用占位报告、历史运行或跳过校验来代替这些证据。

源码版本号、编译成功、镜像上传成功和产品功能验收是不同事实，应分别报告。
