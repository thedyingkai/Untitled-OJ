# Installer

本目录是 OJOS Root Installer / Runtime Manager 的正式源码入口。

当前兼容实现仍保留在 `kernel/installer/*`，迁移映射如下：

- `kernel/installer/core` -> `installer/core`
- `kernel/installer/service` -> `runtime/manager`
- `kernel/installer/cli` -> `installer/cli`
- `kernel/installer/tui` -> `installer/tui`
- `installer/gui` 为原生 GUI 入口，不使用浏览器、WebView 或 Electron。

旧路径只作为 legacy compatibility 保留，新的文档和命令以 Service-first 对象为准。
