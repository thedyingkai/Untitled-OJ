# 模块测试指南

模块提交前必须先跑本地 SDK 检查：

```powershell
cargo run -p ojosctl -- module validate modules/sample-hello/module.yaml
cargo run -p ojosctl -- module package modules/sample-hello -o .tmp/agent/scratch/sample-hello.ojosmod
cargo run -p ojosctl -- module verify .tmp/agent/scratch/sample-hello.ojosmod
```

针对 live Docker control plane 的兼容性验收：

```powershell
powershell -NoProfile -File scripts\e2e-module-compat.ps1 `
  -BaseUrl http://localhost:8080/api `
  -AdminUsername admin1 `
  -AdminPassword admin123 `
  -UserUsername user1 `
  -UserPassword user123
```

该 harness 验证：

- scaffold、validate、package、verify。
- installer dry-run/apply、enable、disable、uninstall dry-run。
- Runtime Snapshot、menu、permission、topology、route viewer。
- runtime service metadata 和 metadata service plan blocking。
- `include_disabled=true` 管理员检查。
- 普通用户 403、无 token 401。
- `path_leaks=0`。

生成的脚手架、package、plan 和报告只能放在 `.tmp/agent/` 下，不能提交。
