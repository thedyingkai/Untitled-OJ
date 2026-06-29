# Contest Core 实现前门禁

> 文档状态：规划门禁，不是实现记录
> 最后更新：2026-06-27

## 门禁结论

建议把 Contest Core 作为第一个真实业务模块，但下一步只能实现 **Contest Core Skeleton**。

## 推荐第一阶段范围

- `modules/contest-core/` manifest using Module Contract v1.
- Metadata install、package、verify、enable 和 disable 路径。
- 如果接受 deployment allowlist 工作，可增加最小 `contest-api` service skeleton。
- Admin/public 占位 route metadata。
- Permission keys：`contest.view`、`contest.participate`、`contest.manage`。
- Runtime snapshot, route table, services and topology visibility.
- API smoke 只覆盖 health 和占位 endpoints，并验证正确 `401`/`403` 行为。

## 明确不做

- 完整 Contest API。
- 完整 Contest frontend。
- 真实 scoreboard。
- 滚榜。
- 复杂封榜窗口。
- Clarification.
- Print.
- Balloon.
- Team management。
- Remote OJ.
- Rating.
- 高级反作弊。
- 将 Judge Core 描述为通用可用能力。

## Kernel 前置条件

如果 skeleton 保持在 Module Contract v1 内，不需要 Kernel core change。`contest-api` 可能需要 trusted compose service allowlist 更新，但这不是 manifest escape hatch。

## 必需验收命令

```powershell
powershell -NoProfile -File scripts\acceptance-kernel.ps1 -SkipDockerBuild
powershell -NoProfile -File scripts\verify-static.ps1 -SkipDockerBuild
powershell -NoProfile -File scripts\e2e-api.ps1 -BaseUrl http://localhost:8080/api -AdminUsername admin1 -AdminPassword admin123 -UserUsername user1 -UserPassword user123 -WorkerToken $env:OJOS_WORKER_TOKEN
powershell -NoProfile -File scripts\e2e-module-compat.ps1 -BaseUrl http://localhost:8080/api -AdminUsername admin1 -AdminPassword admin123 -UserUsername user1 -UserPassword user123
```

## 回滚策略

- Disable `ojos.contest-core`，移除 active menus、permissions 和 routes。
- 如果引入了 trusted `contest-api` compose service，则停止或移除它。
- 保留 module registry history 和 audit entries。
- 不删除 Judge Core 或共享 problem/submission data。

## 最终判断

回归检查通过后，该规划门禁足以支持进入 skeleton 阶段；它不足以宣称 Contest 具备真实业务能力。
