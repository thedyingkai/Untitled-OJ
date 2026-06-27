# Module Installer Threat Model

> 文档状态：当前实现，v0 hardening 边界
> 适用范围：安全审计 / 模块开发 / 部署评审
> 最后更新：2026-06-27

## 资产

Module Installer 保护以下资产：

- `modules/*/module.yaml` 中的模块 manifest。
- `.ojosmod` 本地模块包和包内 `checksums.sha256`、`package.yaml`。
- Module Registry 表、module installation 状态和 module operation history。
- `permission_audit_logs` 中的审计记录。
- Rust `module-installer` internal API。
- Gateway Admin API。
- PostgreSQL 凭据和 `MODULE_INSTALLER_INTERNAL_TOKEN`。

## 攻击面

- `manifest_path` 请求参数。
- `.ojosmod` zip package entry 名称、checksum 和 metadata。
- `module.yaml` 内容、重复字段和危险字段。
- symlink、zip path traversal、绝对路径和路径大小写变体。
- dependency graph、cycle 和 enabled dependent。
- install / enable / disable / upgrade / rollback / uninstall 并发操作。
- Gateway 到 installer internal API 的 token 透传。
- 前端 admin 操作入口和二次确认。

## 威胁

- 路径穿越读取或安装 `modules/` 外的文件。
- symlink escape 或 zip slip 写入、读取宿主敏感路径。
- manifest 携带 `secret`、`token`、`command`、`script`、`hook`、`postinstall` 等危险字段。
- 伪造 admin 或 internal installer 调用绕过权限。
- 操作并发导致 module registry 状态不一致。
- downgrade、rollback 或 uninstall 被滥用破坏核心模块。
- 恶意 package 篡改 checksum 或夹带 `.env`、`.tmp`、`node_modules`、`frontend/dist`、`.git`、`target`。
- operation request/result、audit log 或错误响应泄露 token、secret、DB 连接串或绝对路径。
- Gateway 对 installer 错误透传内部 URL、Rust panic 或 SQL 错误。

## 缓解

- manifest 路径必须是相对路径，canonicalize 后仍位于 repo root 的 `modules/` 下。
- package entry 拒绝绝对路径、`..`、symlink、`.env`、`.tmp`、`node_modules`、`frontend/dist`、`.git`、`target` 和可执行 hook 名称。
- `.ojosmod` 必须包含 `module.yaml`、`checksums.sha256` 和 `package.yaml`，所有非 checksum 文件必须被 checksum 覆盖。
- v0 只做 checksum integrity，不声明发布者可信；signature / trust policy 留到 v1。
- v0 不支持远程模块市场，不安装远程不可信包，不执行 hook，不加载 dynamic frontend bundle。
- Gateway 对 `/api/admin/modules/*` 做 JWT 和 admin / `system.admin` 权限校验；普通用户 403，无 token 401。
- Gateway 到 installer 使用 `X-OJOS-Installer-Token`，该 token 不暴露给前端。
- module operation 使用全局 lock、可配置 TTL 和 PostgreSQL transaction。
- operation request/result 写入前会 redaction，`token`、`secret`、`password`、`authorization` 字段不会原样保存。
- Rust internal API 返回稳定错误结构；Gateway 映射 400/401/403/404/409/503，不向用户返回内部 URL、DB 错误或绝对路径。
- `kernel`、builtin 和 `ojos.judge-core` 受保护；disable/uninstall 默认拒绝。

## 剩余边界

- v0 checksum 只能证明包未被 checksum 列表之外的变更破坏，不能证明发布者可信。
- rollback apply 和 uninstall apply 默认不是通用生产能力；v0 只保留 plan 边界和 demo module metadata 场景。
- distroless runtime image 是后续目标；当前 runtime 已从 Rust builder 镜像收敛到 `debian:bookworm-slim`。
