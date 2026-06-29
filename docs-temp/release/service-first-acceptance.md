# Service-first Acceptance

验收命令：

```powershell
cargo fmt --check
cargo check
cargo test
go test ./...
cd frontend; npm audit --registry=https://registry.npmjs.org --audit-level=high; npm run build; cd ..
powershell -NoProfile -File scripts\e2e-service-runtime.ps1
```

如果本机缺少工具链，最终报告必须列为阻塞项。
