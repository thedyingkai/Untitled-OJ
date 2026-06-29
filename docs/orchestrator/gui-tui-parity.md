# GUI / TUI 等价性

`orchestrator/gui` 和 `orchestrator/tui` 是两个等价入口。GUI 能做的，TUI 必须能做；TUI 能做的，GUI 必须能做。差别只能是交互适配，不是能力差异。

两者共同要求：

- 读取 `orchestrator/schemas/actions.yaml`。
- 读取 `orchestrator/schemas/forms.yaml`。
- 读取 `orchestrator/schemas/plans.yaml`。
- 读取 `orchestrator/schemas/results.yaml`。
- 读取 `orchestrator/schemas/errors.yaml`。
- 调用 `orchestrator/core`。
- 展示 Service、Set、Endpoint、Link、Operation、Topology、LogView 和 DiagnosticReport。
- 不包含 OJ 网站前台或后台业务页面。
- 不各自实现安装、连接、启停、拓扑或诊断逻辑。

GUI 可以使用窗口、表单、列表、图形拓扑和对话框。TUI 可以使用面板、列表、表单、快捷键和文本拓扑。交互形态不同，但 action、输入、计划、结果和错误必须一致。

当前 GUI/TUI 均通过 `orchestrator/core` 读取同一份 Orchestrator view。Operation 页面展示的对象、风险、执行模式、计划要求和摘要来自 core Action Catalog，而不是 GUI 或 TUI 各自推断。字段摘要来自 `forms.yaml`，星号表示必填字段。

Operation 页面展示由 core 生成的共享工作台，包括当前 action、Operation 编号、结构化表单字段、预览目标、预览步骤、是否需要确认、是否可执行、是否可回滚、当前状态、结果状态和日志计数。GUI 以窗口表格展示，TUI 以面板展示；两者都从 `OperationWorkbenchContext` 创建 `OperationWorkbenchSession`，不能各自维护另一套 plan 或执行逻辑。

core 已提供字段更新、确认、执行和回滚的工作台会话函数。GUI 的 action 选择、文本字段编辑、确认、执行和回滚按钮调用这些函数；TUI 的 `n/p` action 切换、`f` 字段切换、`v` 候选值循环、`c/a/u` 确认/执行/回滚快捷键也调用这些函数。入口层只保存当前选择和用户输入，不拥有独立执行语义。确认对话框、快捷键和结果详情可以按 GUI/TUI 形态分别呈现，但状态流必须完全一致。

GUI/TUI 执行 Operation 时都通过 `OperationWorkbenchContext` 把当前 Service、Set、Endpoint、Link 和 Topology 上下文交给 core store。入口层不会自己注册 Endpoint、创建 Link 或应用 Set；这些动作必须由 core executor 根据同一份上下文完成。

当前等价性边界是编排器对象和 Operation 工作台。GUI/TUI 都能加载同一份 schema、Service、Set、Endpoint、Link、Topology 预览，都能在 core session 中完成 action 选择、字段更新、确认、执行和回滚。图形化拓扑布局和更细粒度的表单组件仍可继续打磨，但不能改变 core action、plan、result 和 error 语义。
