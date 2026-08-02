# Storage Service

Storage Service 是业务对象存储 API，不只是一个 Endpoint 描述。它在 8085 提供 bucket 列举/创建、对象 put/get/head/delete 和 metadata 查询，管理 `problems`、`submissions`、`judge-artifacts` 三类 bucket。

后端可使用本地文件系统或 MinIO；生产 Compose 通过独立的 MinIO Service 和 Link 提供对象存储。调用方经 Gateway 的 API route 访问 Storage Service，不应直接持有 MinIO root 凭据。

仓库内 release 使用 local-process：

```text
go run . -f ${OJOS_STORAGE_SERVICE_CONFIG}
```

因此，独立 Orchestrator bundle 只有 manifest 时无法启动它。目标环境还需要 Go 工具链和源码，或改用带可运行 binary/image 的 release。
