# Service SDK

Service SDK 用于生成 `service.yaml`、目录模板和契约测试。

SDK 不生成危险运行配置，不执行仓库脚本，不把 token 写入文件。

示例：

```powershell
cargo run -p ojosctl -- service validate services\gateway\service.yaml
cargo run -p ojosctl -- service package services\gateway -o .tmp\release\gateway.ojos-service
```
