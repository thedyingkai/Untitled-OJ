# Orchestrator Desktop

`ojos-orchestrator-desktop` 是 Windows、Linux 与 macOS 的本地图形入口。它在 Tauri 原生 WebView 中承载与远程控制面相同的 Vue Web UI，不启动外部浏览器，也不使用 iframe。

## 默认模式

```text
Desktop 进程
├─ Embedded Orchestrator backend（127.0.0.1:随机端口）
├─ OS 应用数据目录下的 SQLite
├─ 原生 WebView（加载上述 backend 提供的同源 Web UI）
└─ Managed local execution：Unavailable（使用独立 Agent）
```

Desktop 只监听 loopback，端口由系统分配。启动时生成一次性 bootstrap secret，并在页面初始化阶段提交给 `/api/v1/auth/desktop/exchange`。backend 兑换成功后设置 HttpOnly 本地 admin 会话；脚本只保留内存中的 CSRF 值，bootstrap secret 随即清空，不写入 `localStorage`、文件或 URL。

SQLite 位于操作系统应用数据目录，默认文件名为 `orchestrator.db`；同目录还保存下载 artifact。布局、Store、Topology、Operation 与 Job 都会跨重启保留。Desktop 不会在 SQLite 打开失败时退回内存模式。

Desktop 目前没有可证明的宿主文件到容器 UID/ACL 私密性契约，因此 Windows、Linux 和 macOS 都不会启动内嵌执行 Agent、注册本地 Node、发布 `standard-container-v1` runtime facts 或领取 managed Job。状态明确为 `Unavailable`，并提示注册独立 Agent；这不影响 embedded backend、Manager Web 或 `--daemon-url` 远程管理。该限制不能通过放宽文件权限、提升 Desktop 权限或传入 Registry 凭据绕过。

窗口关闭或应用退出时，Desktop 直接关闭 managed-execution 状态句柄，再关闭 embedded backend。主窗口只允许 daemon origin；本地模式拒绝所有跨 origin 导航和新窗口。

## 构建与运行

从 unsigned portable 包安装到当前用户目录只需要一条原生命令，不需要 MSI、安装脚本或管理员权限：

```powershell
# Windows，在解压后的包目录运行
.\ojos-orchestrator.exe install
# 当前终端使用完整路径；新终端可使用 ojos-orchestrator
& "$env:LOCALAPPDATA\Programs\OJOS-Orchestrator\bin\ojos-orchestrator.exe"
```

```bash
# Linux，在解压后的包目录运行
./ojos-orchestrator install
~/.local/share/ojos-orchestrator/bin/ojos-orchestrator
```

原生安装器同时放置 Desktop、daemon、TUI、Agent、Web UI、schema、Store 索引、service/release manifests、所有 release 引用的 migration，以及本地 release 所需的受跟踪源码。payload 在写入前逐文件校验 SHA-256，安装由目录外独占锁和持久 journal 保护，进程中断后会恢复旧版或发布已经完整验证的 stage。Windows 默认安装到 `%LOCALAPPDATA%\Programs\OJOS-Orchestrator` 并以原生注册表接口加入用户 PATH；Linux 默认安装到 `~/.local/share/ojos-orchestrator`，不修改 shell 配置。重复运行不会删除操作系统应用数据目录中的 SQLite、Agent identity 或 artifact。

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

Windows 构建若产出动态 `WebView2Loader.dll`，portable ZIP 会原样保留；标准 MSVC/Tauri 静态 loader 构建不会伪造或强制要求该 DLL。两种布局都仍需要系统 WebView2 Runtime，现代受支持 Windows 通常已安装。Linux 使用 WebKitGTK，DEB/AppImage/tar 的运行主机仍需安装发行版对应的 WebKitGTK 运行库。

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
| `--data-dir <path>` | 覆盖 SQLite 与 artifact 目录；默认使用 OS 应用数据目录 |
| `--registry-credentials <path>` | 当前拒绝；Registry 凭据必须配置在独立 Agent |
| `--daemon-url <url>` | 使用已有 daemon，关闭 embedded backend |

`--registry-credentials` 保留为兼容的命令行名称，但 Desktop 会在读取文件前拒绝它，并提示在独立 Agent 配置凭据。这样不会让凭据或一个不可证明安全的本地执行器进入 Desktop 进程。

`OJOS_DESKTOP_SMOKE=1` 只供 CI 使用：同源页面首次加载完成后退出，用于验证 WebView、静态资源、bootstrap session、内嵌 backend 与资源布局；不应作为日常启动方式。候选门禁同时设置 `OJOS_DESKTOP_SMOKE_DURATION_MS=1800000`，在真实 Tauri WebView 内连续探测认证能力接口、应用 shell 和事件循环 30 分钟；该值必须是整数且不得超过一小时，未启用 smoke 时禁止单独设置。
