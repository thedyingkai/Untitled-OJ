# Product repository boundary

This Git repository contains the OJOS product: runtime source, public contracts,
generated production SDKs, build dependencies, packaging, deployment assets and
product documentation. Push changes only to `main`; do not create Codex branches.

All software verification code belongs outside this repository. On this machine
the independent suite is at `D:\Untitled-OJ-external-tests`. Never add unit tests,
inline test modules, mocks, fixtures, end-to-end drivers, load generators, smoke
modes or drill scripts anywhere inside this worktree, even as ignored files.
Do not generate them from product scaffolding or SDK generators. Do not put test
code or test execution in GitHub workflows. Workflows may compile, package and
publish product artifacts and documentation; artifact integrity checks remain
part of packaging.

Run verification against current product sources copied into a disposable
directory under the external suite. Keep suite dependencies and reports there.
Never overlay tests into this worktree. Record which checks ran and which need
external services; compilation is not proof that integration scenarios passed.

OJ problem test cases, problem-package validation, judge execution, runtime
health checks and signature verification are product features, not software
verification assets. Preserve those features when enforcing this boundary.

Keep transport adapters, domain rules and persistence separate. Follow the
dependency direction documented in `docs/architecture/README.md`. Preserve existing
worktree changes, keep refactors scoped, and do not rewrite Git history.
