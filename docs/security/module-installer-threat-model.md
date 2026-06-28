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

## Hotplug L1 Gateway Dynamic Proxy 威胁

新增攻击面：

- Manifest 声明的 `gateway_routes.prefix`。
- Manifest 声明的 `service_id`。
- Runtime route table reload 与 cache replacement。
- Dynamic proxy path rewrite。
- Gateway 向可信内部服务传播 actor 信息。

主要威胁：

- 如果 manifest 可声明 arbitrary `target_url`，会产生 SSRF。
- 模块占用 `/api/auth`、`/api/admin/modules`、`/api/admin/health`、`/api/health`、`/api/internal` 或 `/api/judge/worker` 等 reserved prefix。
- 弱 `auth_mode` 映射导致权限绕过。
- 原始 `Authorization`、internal URL 或 upstream error 泄露给模块服务或用户。
- duplicate/overlap prefix 造成路由冲突歧义。

缓解：

- Manifest 只能引用 `service_id`；Gateway 通过 trusted configuration 解析。
- `target_url`、remote URL、command、script 和 hook 均禁止。
- Runtime Route Table 阻断 reserved prefix、duplicate prefix、unknown service 和 unsupported auth mode。
- Core static routes 对 `/api/auth` 和 `/api/judge/worker` 保持优先级。
- Dynamic proxy 去除 hop-by-hop headers，默认不转发原始 `Authorization`。
- Gateway 向 trusted service 转发 sanitized actor headers 和 internal HMAC headers。
- Admin route table 默认隐藏 `upstream_base`；普通用户不能访问 route table API。

## Hotplug L2 Runtime Driver 威胁

新增攻击面：

- Manifest 声明的 services 和 workers。
- `compose_service` identity。
- Runtime plan generation API。
- Service health 对 Gateway route availability 的影响。

主要威胁：

- Gateway、Web Shell 或 installer 若能访问 Docker socket，会获得 host control。
- Manifest 若能自动应用 `command`、`script`、`image`、`host_path`、`mount`、`privileged` 或 `cap_add`，会造成任意代码执行。
- Service health 若接受 arbitrary URL，会造成 SSRF 或内部探测。
- Unavailable dynamic route 若先返回错误再鉴权，可能泄露路由存在性。

缓解：

- Gateway、Web Shell 和 module-installer 均不挂载 Docker socket。
- L2 foundation 默认只做 plan/status/health，Gateway apply-plan 禁用。
- Compose driver 使用 trusted Gateway service configuration 和 allowlist。
- Installer Core 拒绝 dangerous runtime fields。
- Runtime plan 使用 structured command arrays，不使用 shell string。
- Dynamic proxy 先执行 auth mode，再返回 service unavailable。
- Admin API 不返回 host path、raw env、DSN、token 或 internal service URL。

## Hotplug L2 Controlled Apply 威胁

新增攻击面：

- 传给 `ojosctl` 或 operator 的 runtime plan JSON。
- 本地 compose apply 执行。
- Operation lock、operation history 和 audit 写入。
- Plan TTL 与确认流程。

主要威胁：

- 如果 plan command 是 shell string，会造成任意命令执行。
- Gateway/Web/module-installer 若能执行 Docker 或访问 Docker socket，会获得 host control。
- Plan 若能选择任意 compose file 或 service name，会越过 trusted runtime 边界。
- 旧 plan replay 可能绕过新的 topology/service policy。
- stdout/stderr、env、audit log 或 operation result 可能泄露 secret。
- 并发 apply 可能破坏 runtime state。

缓解：

- `ojosctl` 在 apply 前校验 argv-only command、driver、action、TTL、service id、target allowlist 和 fixed compose path。
- Real apply 必须 `--confirm`；`--dry-run` 不执行。
- Gateway/Web apply 明确禁用，并对 admin apply 尝试返回 501。
- Compose execution 使用 argument array 和 trusted service allowlist。
- Apply 使用 service lock、TTL 和 command timeout。
- stdout/stderr 与 operation request/result 写入前裁剪和脱敏。
- Manifest 不能声明 command/script/image/mount/privileged/capability 作为可执行 runtime 指令。
