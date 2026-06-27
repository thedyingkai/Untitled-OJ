# Regression Matrix

| Area | Verification | Current status | Failure handling | Blocks next stage |
| --- | --- | --- | --- | --- |
| Auth | `scripts/e2e-api.ps1`, Go tests in `services/auth` | Required green | Fix login/token/permission checks | Yes |
| Problem API | `scripts/e2e-api.ps1`, Go tests in `services/problem-api` | Required green | Inspect API reports and packagefs tests | Yes |
| Judge API | `scripts/e2e-api.ps1`, Go tests in `services/judge-api` | Required green | Inspect submission/worker-link reports | Yes |
| Judge Worker | `services/judge-worker` Rust tests | Required green | Fix package loading/runtime test failures | Yes |
| Installer | Rust root tests and `verify-static` | Required green | Fix manifest/package/install planning regressions | Yes |
| Module Runtime | Gateway Go tests and e2e runtime snapshot checks | Required green | Fix snapshot/table/topology derivation | Yes |
| Dynamic Proxy | Gateway proxy tests and e2e dynamic route checks | Required green | Fix trusted service/auth/header boundaries | Yes |
| Controlled Apply | `ojosctl runtime apply-plan --dry-run`, optional confirm smoke | Dry-run required, confirm opt-in | Fix allowlist/lock/history/redaction | Blocks apply work |
| Module SDK | `ojosctl module init`, package/verify tests | Required green | Fix scaffold/schema/package drift | Yes |
| Sample Module | `scripts/e2e-module-compat.ps1` | Required green | Fix sample manifest or registry flow | Yes |
| Frontend Shell | `npm run build`, e2e contribution checks | Required green | Fix build or contribution rendering | Yes |
| Docs | Docs index/status and release docs | Required current | Update stale status before feature start | Yes |
| Security scan | `verify-static`, secret/path scans | Required green | Remove leak/secret before commit | Yes |
| Path leak scan | e2e summaries | Must be `0` | Redact output and API responses | Yes |
| Permission rejection | e2e user/no-token checks | User `403`, no token `401` | Fix middleware/admin boundary | Yes |
