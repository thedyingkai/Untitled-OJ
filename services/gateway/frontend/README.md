# Gateway 业务前端

这个目录是 OJ 站点的业务 UI，使用 Vue、Vite、Pinia 和 Naive UI。它通过 Gateway 调用登录、题目、提交和结果等业务 API。

它不属于 Orchestrator 控制面。安装、Link、拓扑和 Operation 由 `manager/web` 或 TUI 管理；业务前端不能直接修改这些对象。

常用命令：

验证命令和场景已迁至仓库外的独立验证目录；不在产品工作树内运行。

Playwright E2E 需要先安装 Chromium：

验证命令和场景已迁至仓库外的独立验证目录；不在产品工作树内运行。
