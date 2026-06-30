# Frontend

该目录实现 Web Shell 业务 UI。Web Shell 是 Service，不是 OJOS Orchestrator。

Web Shell 通过 Gateway 访问业务 API，只读展示必要的 Service 状态和 Topology 信息；安装、连接、拓扑变更和 Operation apply 由 Orchestrator GUI/TUI 负责。
