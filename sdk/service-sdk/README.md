# Service SDK

Service SDK 只生成 `service.yaml`、最小目录结构和契约测试，不执行仓库脚本，不写入明文 secret，不生成 privileged、cap_add 或 host mount 配置。

推荐命令：

```powershell
cargo run -p ojosctl -- service validate services\gateway\service.yaml
cargo run -p ojosctl -- service package services\gateway -o .tmp\release\gateway.ojos-service
```
