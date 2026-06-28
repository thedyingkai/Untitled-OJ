# Judge Core Readiness

Judge Core 是 OJOS 当前第一个核心 feature module，提供题库、提交、评测、Worker Link、结果存储和评测管理能力。

## 当前状态

- Judge Core 在 Module Registry 中表示为 `ojos.judge-core`。
- Judge Core 出现在 Runtime Snapshot、runtime routes、runtime services 和 topology 中。
- Judge Core 声明 `problem-api`、`judge-api` 和 `judge-worker` 服务/Worker。
- Judge Core route metadata 参与 dynamic route table 校验，同时保留兼容静态路由。
- Judge Core disable/uninstall 继续受保护。

## 不标记 GA

Judge Core 当前不标记 GA，仍缺少：

- 真实多机 Worker 部署验收。
- 跨主机网络故障与恢复验收。
- 时钟漂移和 lease 边界验收。
- 长时间 soak test。
- package signature 和 trust policy。
- 完整 service deployment hotplug automation。

## 后续门禁

Judge Core 可作为 Kernel/runtime 回归基线使用，但不能作为生产 GA 能力宣传。文档、release notes 和 UI 不得写成 Judge Core GA。
