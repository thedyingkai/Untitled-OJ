# Release 文档

本目录用于保存发布门禁、发布记录和发布限制。发布结论不能把自动化命令当作唯一验收方法，也不能声明未核实能力已经完成。

发布前必须核对：

- 正式文档结构。
- 核心对象模型。
- Orchestrator 数据库 schema。
- GUI/TUI Action Registry。
- Service/Set 样例。
- Endpoint/Link 行为。
- 编译和测试结果。
- 人工操作证据。
- 变更报告。
- 无真实 secret。
- 无本机路径泄漏。
- 无 tracked 构建产物或本地依赖目录。

发布记录必须说明当前仍未覆盖的能力边界。没有证据的能力不得写成已完成。

当前可核对证据见 [可核对证据](evidence.md)。
