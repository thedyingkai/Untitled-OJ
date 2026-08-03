# Orchestrator Desktop

`ojos-orchestrator-desktop` 是 Windows 与 Linux 的本地图形入口。它在 Tauri 原生 WebView 中承载与远程控制面相同的 Vue Web UI，不启动外部浏览器，也不使用 iframe。

## 默认模式

```text
Desktop 进程
├─ Embedded Orchestrator backend（127.0.0.1:随机端口）
├─ loopback Agent（与远程 Node 使用同一 Job/lease/runtime 语义）
├─ OS 应用数据目录下的 SQLite
└─ 原生 WebView（加载上述 backend 提供的同源 Web UI）
```

Desktop 只监听 loopback，端口由系统分配。启动时生成一次性 bootstrap secret，并在页面初始化阶段提交给 `/api/v1/auth/desktop/exchange`。backend 兑换成功后设置 HttpOnly 本地 admin 会话；脚本只保留内存中的 CSRF 值，bootstrap secret 随即清空，不写入 `localStorage`、文件或 URL。

SQLite 位于操作系统应用数据目录，默认文件名为 `orchestrator.db`；同目录还保存 `agent-ledger.db` 和下载 artifact。布局、Store、Topology、Operation、Job 与本地 Agent 幂等账本都会跨重启保留。Desktop 不会在 SQLite 打开失败时退回内存模式。

窗口关闭或应用退出时，Desktop 停止接收新任务，通知 loopback Agent 排空，并在最长 30 秒后关闭 embedded backend。主窗口只允许 daemon origin；本地模式拒绝所有跨 origin 导航和新窗口。

## 构建与运行

开发环境先生成 Web UI，再从仓库根目录启动：

```bash
npm --prefix manager/web ci
npm --prefix manager/web run typecheck
npm --prefix manager/web test
npm --prefix manager/web run build
cargo run -p ojos-orchestrator-desktop
```

`--repo-root` 与 `--web-root` 只用于开发或诊断覆盖。MSI、DEB 与 AppImage 会从 Tauri 的安装资源目录读取 schema、service/release manifest、sets、Store index 和 Web build；portable ZIP/tar 会从 Desktop 可执行文件旁的包根目录发现同一布局。因此发行包不依赖当前工作目录，也不需要传 `--repo-root`：

```powershell
# Windows portable ZIP
.\bin\ojos-orchestrator-desktop.exe
```

```bash
# Linux portable tar.gz
./bin/ojos-orchestrator-desktop
```

Windows portable ZIP 同时包含 `bin/WebView2Loader.dll`。Windows 还需要 WebView2 Runtime；现代受支持版本通常已安装。Linux 使用 WebKitGTK，DEB/AppImage/tar 的运行主机仍需安装发行版对应的 WebKitGTK 运行库。

发布流水线会分别从 portable、MSI/DEB 与 AppImage 的实际资源布局启动 Desktop，并在不传 `--repo-root`/`--web-root` 的情况下完成 WebView、bootstrap exchange、静态资源和 v1 API smoke。

## 连接已有 daemon

如需让 Desktop 窗口管理已有控制面，而不是启动本地 backend：

```bash
ojos-orchestrator-desktop --daemon-url https://orchestrator.example.com
```

- URL 必须指向 origin 根，不能携带用户名、密码、查询串或 fragment。
- loopback 可使用 HTTP；任何非 loopback 控制面必须使用 HTTPS。
- Desktop 不注入 bearer token。远程页面使用 daemon 的 OIDC Authorization Code + PKCE 与 HttpOnly 会话。
- 创建窗口前，Desktop 从 `/api/v1/auth/config` 读取 `authorization_endpoint`，只额外允许该配置给出的精确 HTTPS origin。OIDC callback 回到 daemon origin；未知 origin 与所有新窗口仍被拒绝。
- 身份配置读取失败或 OIDC endpoint 不合法时，Desktop 拒绝创建一个无法登录的远程窗口。

## 参数

| 参数 | 说明 |
| ---- | ---- |
| `--repo-root <path>` | 开发/诊断覆盖；默认自动发现安装资源或 portable 包根 |
| `--web-root <path>` | 开发/诊断覆盖；默认使用已打包的 `manager/web/dist` |
| `--data-dir <path>` | 覆盖 SQLite、Agent ledger 与 artifact 目录；默认使用 OS 应用数据目录 |
| `--registry-credentials <path>` | 仅 embedded 模式；为本地 loopback Agent 提供严格 schema v1 的私有 Registry 凭据文件 |
| `--daemon-url <url>` | 使用已有 daemon，关闭 embedded backend 与 loopback Agent |

`--registry-credentials` 与远程 Agent 使用同一 64 KiB/32 Registry 严格 JSON 契约；凭据只进入原生 Docker runtime，不注入 WebView、URL、`localStorage` 或日志。Desktop 启动时先校验文件并 fail closed；文件轮换后应重启 Desktop。该参数与 `--daemon-url` 互斥，因为外部 daemon 的 Registry 凭据由其 Node Agent 管理。

`OJOS_DESKTOP_SMOKE=1` 只供 CI 使用：同源页面首次加载完成后退出，用于验证 WebView、静态资源、bootstrap session、内嵌 backend 与资源布局；不应作为日常启动方式。候选门禁同时设置 `OJOS_DESKTOP_SMOKE_DURATION_MS=1800000`，在真实 Tauri WebView 内连续探测认证能力接口、应用 shell 和事件循环 30 分钟；该值必须是整数且不得超过一小时，未启用 smoke 时禁止单独设置。
