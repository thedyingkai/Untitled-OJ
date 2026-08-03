# Orchestrator v1 签名候选与晋级政策

本文定义 `.github/workflows/release.yml` 的发布边界。它是可由静态测试核对的政策，
不是一次已经完成的发布记录。

## 当前状态

当前候选状态必须明确记录为：

```text
status: SECURITY_ACCEPTANCE_PENDING
published: false
```

这表示候选功能、容量或签名门禁即使全部通过，也没有授权对外发布。当前不得创建
GitHub Release，也不得把候选 manifest 的 `published` 改成 `true`。安全验收完成前，
文档、版本号、tag 和 Actions artifact 都不能被表述为 GA 发布。

## 唯一工作流与两种显式模式

`release.yml` 只接受手工 `workflow_dispatch`，不得监听 tag push。创建 `v1.0.0` tag
本身不会构建、签名或发布任何内容。

候选构建模式使用 `publish=false`。它运行功能、升级、容量证据、Windows/Linux 构建、
签名和候选组装，最后只上传 `orchestrator-v1-signed-candidate` Actions artifact；不得调用
`gh release create`。候选只接受 workflow 的首次 attempt；对同一 run 执行 rerun 会被拒绝，
修复后必须以新 run 重新形成候选。候选入口还会从 GitHub Commit API 核对完整提交信息必须
精确为 `feat(orchestrator): freeze v1 release candidate`，不能从其他 `main` commit 生成。

晋级模式必须显式使用 `publish=true` 并提供唯一的 `candidate_run_id`。晋级 job
`promote-existing-candidate` 只能下载该 run 已经产生的候选、复验信任并发布，不得执行
Cargo/npm/Tauri build，不得重新打包、签名、生成 SBOM 或 attestation。发布内容因此与被验收
候选保持字节级一致。

## 受保护环境与准确执行入口

候选构建前，仓库管理员必须创建 `orchestrator-rc-signing` GitHub Environment，并在该
Environment 中配置：

- secrets：`AZURE_CLIENT_ID`、`AZURE_TENANT_ID`、
  `AZURE_SUBSCRIPTION_ID`；
- variables：`AZURE_ARTIFACT_SIGNING_ENDPOINT`、
  `AZURE_ARTIFACT_SIGNING_ACCOUNT`、`AZURE_ARTIFACT_SIGNING_PROFILE`、
  `WINDOWS_PUBLISHER_SUBJECT`。

Azure 身份必须使用 GitHub OIDC federation，federated credential subject 必须精确为
`repo:OWNER/REPOSITORY:environment:orchestrator-rc-signing`，且服务主体只能授予目标 Artifact
Signing certificate profile 的 signer 权限。`WINDOWS_PUBLISHER_SUBJECT` 必须与该 profile
实际签出的证书 subject 完全一致；空值、显示名称或推测值都会使结构化 Authenticode 复核失败。
候选开始前还必须已经存在同一 SHA 的成功 production capacity artifact；`release.yml` 会自行
下载并重新验证，不能用 run ID 参数绕过。

在 `main` HEAD、完整提交信息和 production evidence 均已核对后，只能用以下候选入口：

```bash
export GITHUB_REPOSITORY=OWNER/REPOSITORY
gh workflow run release.yml \
  --repo "$GITHUB_REPOSITORY" \
  --ref main \
  -f version=1.0.0 \
  -f publish=false
```

本命令不接受 `candidate_run_id`。运行必须为首次 attempt；失败后不能 rerun，任何代码、文档、
workflow 或制品修复都要生成新的候选 SHA，从 OCI 部署和 24 小时容量门禁重新开始。

安全验收完成后，管理员还要创建 `orchestrator-ga-promotion` Environment，并配置下文列出的
五个精确候选身份变量。只有届时才允许执行：

```bash
gh workflow run release.yml \
  --repo "$GITHUB_REPOSITORY" \
  --ref main \
  -f version=1.0.0 \
  -f publish=true \
  -f candidate_run_id="$CANDIDATE_RUN_ID"
```

当前目标明确延后安全验收和正式发布，因此上述 promotion 命令只定义未来入口，不得在当前
候选阶段执行。

## 候选文件集合

`candidate/payload/` 必须恰好包含 11 个主制品及其 11 个 Sigstore bundle，共 22 个文件。

Windows x64 的 5 个主制品是 MSI、portable ZIP、SPDX SBOM、provenance 和
`SHA256SUMS`；Linux x86_64 的 6 个主制品是 DEB、AppImage、portable tar.gz、SPDX SBOM、
provenance 和 `SHA256SUMS`。不允许缺项，也不允许把日志、manifest 或其他附件混入 payload。

每个主制品都必须同时具备：

- 对应的 `<name>.sigstore.json` Cosign bundle；
- 绑定候选 commit 与 `release.yml` 身份的 GitHub build provenance attestation；
- 平台 checksum 覆盖；
- Windows 可执行文件、MSI 及 portable/MSI 内副本还必须通过 Authenticode 验证。

Windows 签名使用 GitHub OIDC 登录 Azure，再调用 Azure Artifact Signing。顺序固定为：先以
`--no-bundle` 构建四个 EXE，OIDC 登录后签署四个 EXE，再从已签名 Desktop 生成 MSI、签署
MSI，最后从已签名文件打包 portable ZIP 并运行 Authenticode 复验。不得使用长期 Azure
client secret，也不得把未签名二进制放入 MSI 或 portable 包。

OJOS 发布者签署的四个 EXE、MSI 及其包内副本必须从嵌入的 PKCS#7 中解析出 RFC3161
TSTInfo，验证 timestamp token 签名及其对父 Authenticode 签名的 messageImprint 绑定，并强制
SHA-256；不能用固定字符串或仅凭存在时间戳证书作为证据。`WebView2Loader.dll` 必须保留有效的
Microsoft 原签名且不能用 OJOS 证书重签；它的 vendor timestamp 按实际情况记录为 `None`、
`AuthenticodeLegacy` 或 `RFC3161`，不把供应商的历史签名策略伪写为 OJOS 的发布策略。签名后
还必须分别从 MSI 安装布局和 portable ZIP 启动一次，不能只检查压缩包结构。

## 生产容量候选镜像

`.github/workflows/orchestrator-candidate-images.yml` 只允许从 `main` 创建与当前 commit
绑定的生产容量镜像。提交信息精确为 `feat(orchestrator): freeze v1 release candidate` 时自动
触发；手工入口仅用于对同一 `main` commit 进行受控重跑。它必须构建并推送恰好三个 Linux
x64 镜像：control-plane、Agent 和 capacity fixture。三者都注入同一
`GITHUB_SHA`/`OJOS_BUILD_COMMIT`，并写入
`org.opencontainers.image.revision`；拉回镜像后必须核对该 label 与 commit 完全一致。

Capacity fixture 的基础镜像输入必须是小写 `name@sha256:<64 hex>` RepoDigest，禁止 tag。
每个候选镜像同时启用 BuildKit `provenance: mode=max`、SBOM 和独立的
`actions/attest-build-provenance`，最终 evidence 记录 component、image、digest、完整 digest
reference、commit SHA 与 workflow run ID。容量报告中的 `source_commit`、`oci_revision`、
`provenance_commit` 和实际 server build 必须全部等于桌面/服务候选 SHA；仅 tag 相同不构成
同候选证据。

production report 必须由 workflow 首次 attempt 产生，并携带三个有 digest 的 NDJSON sidecar：
capacity events、Prometheus snapshots 和 `environment_observations_ndjson`。环境观察必须按
qualification、每个 Operation round、final 的顺序一一覆盖；首个 Operation round 是暖机后的
环境基线，10 worker/100 Engine、2,000 个真实容器以及 2,000 Endpoint/8,000 Link 的资源身份
摘要在全过程保持稳定。

## Manifest 和证据边界

`candidate/candidate-manifest.json` 只是一份候选证据索引。schema v2 记录候选 SHA、候选/容量
run、候选 workflow attempt（必须为 1）、
11/11/22 基数、每个 payload 文件的 digest/大小以及证据摘要，并固定保存
`SECURITY_ACCEPTANCE_PENDING` 与 `published=false`。

`candidate/evidence/` 保存容量和 Authenticode 等复核材料。Manifest 与 evidence 可以包含在
Actions 候选 artifact 中供验收，但二者都不得传给 `gh release create`。晋级时只允许上传已经
复验的 `candidate/payload/` 22 个文件；manifest 不能被当成签名制品，也不能作为发布状态真值。

候选上传后，workflow 还会生成独立的 `orchestrator-v1-candidate-identity` 证据 artifact，记录
候选 SHA/run/attempt、candidate manifest SHA-256、Actions artifact ID、REST API 返回的
`sha256:` digest 和 upload action 返回的 digest。该文件用于安全验收登记，不属于 22 文件
Release payload，也不能直接授权晋级。

安全验收后，受保护的 `orchestrator-ga-promotion` Environment 必须同时配置以下五个变量，且值
来自上述候选身份记录：

- `ORCHESTRATOR_SECURITY_ACCEPTED_CANDIDATE_SHA`；
- `ORCHESTRATOR_SECURITY_ACCEPTED_CANDIDATE_RUN_ID`；
- `ORCHESTRATOR_SECURITY_ACCEPTED_CANDIDATE_MANIFEST_SHA256`；
- `ORCHESTRATOR_SECURITY_ACCEPTED_CANDIDATE_ARTIFACT_ID`；
- `ORCHESTRATOR_SECURITY_ACCEPTED_CANDIDATE_ARTIFACT_DIGEST`（含 `sha256:` 前缀）。

只登记 SHA 不构成验收；run、manifest 或 artifact 任一身份不同都必须重新验收。

## 晋级时的复验

晋级 job 至少必须完成以下检查：

1. 要求候选和晋级 workflow 均为首次 attempt，并从 Actions REST API 要求该 run 恰好存在一个
   未过期、名称精确匹配且命中全部五个受保护验收值的候选 artifact；
2. 按精确 artifact ID 下载原始 ZIP，在解包前校验其 SHA-256 等于 REST `artifact.digest`，再校验
   manifest SHA-256 等于受保护验收值；
3. 校验 manifest 的候选 SHA、workflow run ID/attempt、`published=false` 和 11/11/22 基数；
4. 对 11 个主制品逐一校验 digest、Cosign bundle 与 GitHub attestation；
5. 校验两个平台的 `SHA256SUMS`；
6. 要求既有不可变 tag 指向 manifest 中的候选 SHA，并使用 `--verify-tag`；
7. `gh release create` 的资产列表只能来自 `candidate/payload/`。

任一步失败都必须停止晋级，不能退回“重新构建一次”、跳过签名或手工拼接 Release 资产。
