# Web UI、TUI 与 daemon

编排器有三个正式入口，全部经由同一套 `services/orchestrator/core` 与 `platform/schemas/orchestrator` 契约：

| 入口 | 位置 | 形态 | 定位 |
| ---- | ---- | ---- | ---- |
| Web UI | `manager/web`（daemon 托管） | 浏览器 | 主入口：画布拓扑编辑、插件商店、服务生命周期、操作审计 |
| TUI | `manager/tui` | 终端 | 无浏览器的服务器场景 |
| daemon HTTP API | `services/orchestrator/backend` | REST | 自动化与其他入口的统一后端 |

原 egui GUI 已删除。文件名为兼容旧链接保留，正文只描述当前入口。

## 能力边界

- 所有变更动作都走 `OrchestratorActionConsole` → `OrchestratorActionDispatcher`。Web UI 通过 REST 提交
  `ActionRequest`，TUI 在进程内调用。入口不能自行修改 store 或 Operation 状态。
- 画布坐标是 Web UI 状态，经 `/ui/layout` 写入 `.ojos/ui-layout.json`，不进入编排 store。商店安装最终
  调用 `import_external_release` 和 `release.install`；Link 与 Host 操作也由 core 执行。
- TUI 快捷键：

  ```text
  页签：1-9 core 视图 / 0 商店
  Endpoint Actions: e create  E update  x delete  h health check
  Link Actions: l create  L update  X delete  H health check  k enable  K disable
  Host Actions: s host start  S host stop  w execute_service_driver
  Release Actions: i install  R create  U update  Y delete  B rollback  z validate
  Store Actions: m install  M manual install  g reload index
  Operation Actions: c confirm  a apply  u rollback  o logs
  Diagnostics: d create  D export markdown
  上下方向键在商店页选模块、在 Endpoint 页选端点、在 Service 页选 Release 行
  （主机启停以选中端点所属 host 为目标，Release 快捷键以选中 Release 行为目标）
  ```

- TUI 的商店页只读**本地索引文件**（`OJOS_STORE_INDEX_URL` 指向仓库内相对路径，默认 `store/index.json`）；索引是 http(s) 地址时提示改用 daemon/Web UI——TUI 不引入 HTTP 客户端依赖。模块的**导入与安装**本身走 core，可接受通过出站安全校验的 HTTP(S) 或允许的本地来源。TUI 表单没有 `external_service_running` 选项；登记外部已运行服务应使用 Web UI 或 HTTP API。
- `w` 控制 `execute_service_driver`，默认关闭。`B` 在未授权时直接拒绝；`Y` 只删除未被部署引用的 Release
  记录，不执行运行时驱动。`i` 只在打开授权后执行驱动；首次安装可以不带授权只写编排状态，但升级正在运行的
  固定运行时必须授权，避免留下旧进程。生命周期执行和回滚也需要逐次授权，Web UI 遵循同一规则。
- Release 快捷键不会继承 workbench 的 `gateway`、`0.1.0` 或示例路径。`i`、`Y` 使用当前 Release 的真实
  Service 和版本；`R`、`U`、`z` 只带当前 Service。`B` 不预填版本，由 core 选择该 Service 最近一次成功安装，
  也可在完整表单中显式填写版本或目标 Operation。

## Store 选择

没有 `ORCHESTRATOR_DATABASE_URL` 时，入口从仓库 manifest 构建本地视图，并使用
`MemoryOrchestratorStore`。商店导入、布局之外的拓扑变更和 Operation 都会在进程退出后丢失。

在设置了 `ORCHESTRATOR_DATABASE_URL` 时，使用 `PgOrchestratorStore`，商店导入的 Service/Release 持久化到编排器数据库。数据库不可用时入口返回错误，而不是回退到仓库 fixture。

Web UI / TUI 不包含 OJ 业务后端功能、不充当 Gateway。
