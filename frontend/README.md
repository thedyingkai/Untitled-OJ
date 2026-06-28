# OJOS Web Shell

本目录是 OJOS 浏览器管理界面。它负责登录、题库、提交、评测状态、模块管理视图、Runtime Snapshot、路由、服务、操作历史、拓扑和权限展示。

Web Shell 不是官方安装器入口，不执行模块安装、启用、禁用或 runtime apply。正式安装和运维入口是 `ojosctl` 与 `ojos-installer-tui`。

## 开发命令

```powershell
npm install
npm run build
```

本地开发使用 `frontend/.env` 中的 `VITE_API_BASE_URL`。构建产物写入 `frontend/dist/`，该目录不能提交。
