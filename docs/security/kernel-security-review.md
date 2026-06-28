# Kernel Security Review

日期：2026-06-28

本文记录 v0.1.0 发布基线的 Kernel、Gateway、Runtime、Installer 和 Module SDK 安全边界。

## Dynamic Gateway Proxy

- Manifest route 只能引用 `service_id`，不能提供 arbitrary upstream URL。
- Gateway 通过 trusted configuration 解析 `service_id`。
- Dynamic route 不能占用 reserved prefix。
- Core static routes 优先于 dynamic routes。
- Unknown service、disabled route、conflict route 不会被 proxy。

Reserved prefixes：

```text
/api/auth
/api/admin/modules
/api/admin/health
/api/health
/api/internal
/api/judge/worker
```

## Header 与 Auth 边界

- Dynamic proxy 默认不转发原始 `Authorization`。
- Gateway 转发受控 actor headers 和 internal HMAC headers。
- `public`、`user`、`admin`、`worker`、`internal` auth mode 语义明确。
- `worker` 和 `internal` 不是 public dynamic proxy surface。

## Controlled Apply 边界

- Gateway/Web 不执行 runtime apply。
- Gateway/Web/module-installer 不挂载 Docker socket。
- `ojosctl` 或未来 operator 是 controlled apply path。
- Apply 使用 argv array、固定 compose 配置、trusted service allowlist、confirm、dry-run、timeout 和 service lock。
- Operation history 会裁剪和脱敏。

## Package 与 Manifest 边界

- Package v1 只验证 checksum integrity，不证明 publisher trust。
- Signature / trust policy 未完成。
- Manifest dangerous fields 会被拒绝，包括 `command`、`script`、`hook`、`image`、`mount`、`host_path`、`privileged`、`cap_add`、`target_url`、secret 和 token-like fields。
- Remote module market 和 untrusted hooks 不支持。

## Path Leak 边界

- E2E 脚本扫描响应中的内部路径并汇总 `path_leaks`。
- Public API 不得暴露 host path、Docker socket path、package dir、stdout/stderr path、checker log、DSN 或 secret。
- CLI 默认不输出本机绝对路径；需要排障时才用 verbose。

## 剩余风险

- Dynamic frontend bundle 安全设计未完成。
- Publisher signature / trust policy 未完成。
- True multi-machine runtime apply 未完成。
- Judge Core 长时间 soak test 未完成。
- Full hotplug automation 未完成。
