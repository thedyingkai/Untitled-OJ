> 文档状态：已归档
> 警告：本文档仅保留历史参考，可能包含过时架构或旧部署方式，不可作为当前部署依据。
> 危险提示：本文档可能包含 NATS、privileged true、worker 直连 PostgreSQL/Redis、内部路径暴露等过时内容。当前实现不采用这些方案。

# Judge Resource Limits

This document describes the deployment requirements and acceptance checks for
OJOS judge-worker resource limits.

## Enforced Limits

Each case runs in its own nsjail process and cgroup v2 directory.

| Limit | Mechanism | Result field |
| --- | --- | --- |
| Time | nsjail `--time_limit` plus outer wall timeout | `time_ms` |
| Memory | cgroup v2 `memory.max`; peak from `memory.peak` | `memory_kb` |
| Output | stdout/stderr file size check and `--rlimit_fsize` | `OUTPUT_LIMIT_EXCEEDED` |
| Compile output | compile stdout/stderr file size check | `COMPILE_ERROR` or `OUTPUT_LIMIT_EXCEEDED` message |
| File size | nsjail `--rlimit_fsize` | case status |
| Processes | nsjail `--rlimit_nproc` plus cgroup `pids.max` | `RUNTIME_ERROR` or `SYSTEM_ERROR` |
| Open files | nsjail `--rlimit_nofile` | `RUNTIME_ERROR` |

Status values supported by API and frontend:

```text
ACCEPTED
WRONG_ANSWER
COMPILE_ERROR
RUNTIME_ERROR
TIME_LIMIT_EXCEEDED
MEMORY_LIMIT_EXCEEDED
OUTPUT_LIMIT_EXCEEDED
SYSTEM_ERROR
CANCELLED
UNSUPPORTED_LANGUAGE
PENDING
JUDGING
```

## cgroup v2 Requirement

The worker requires Linux cgroup v2. It fails fast if the configured cgroup root
does not contain `cgroup.controllers`.

Configuration:

```bash
OJOS_CGROUP_V2_ROOT=/sys/fs/cgroup
```

The worker creates per-case directories below:

```text
/sys/fs/cgroup/ojos/judge-worker/{pid}-{timestamp}-{sequence}
```

For each run it writes:

```text
memory.max = memory_limit_mb * 1024 * 1024
pids.max   = 64
cgroup.procs = nsjail_pid
```

After the run it reads:

```text
memory.peak
memory.events
```

If `memory.events` reports `oom`, `oom_kill`, or `oom_group_kill`, the case is
reported as `MEMORY_LIMIT_EXCEEDED`, not `RUNTIME_ERROR` or `SYSTEM_ERROR`.

## Docker Compose Worker Requirements

The default compose file must not enable Docker privileged mode.

Required worker settings:

```yaml
cap_add:
  - SYS_ADMIN
  - SYS_CHROOT
  - SETUID
  - SETGID
  - NET_ADMIN
volumes:
  - /sys/fs/cgroup:/sys/fs/cgroup:rw
environment:
  OJOS_CGROUP_V2_ROOT: /sys/fs/cgroup
```

The worker node compose is `deploy/worker/docker-compose.yml`. It does not
mount `storage/problems` or `storage/submissions`; source code and problem
packages are downloaded through the Worker Artifact API and verified by sha256.

`SYS_ADMIN` is required by nsjail namespace and mount setup. `SYS_CHROOT`,
`SETUID`, and `SETGID` are required for the jail user transition. `NET_ADMIN` is
kept for nsjail network namespace setup in the current container profile.
The default compose files do not enable Docker privileged mode and do not
disable seccomp or AppArmor. If a particular distribution needs a custom LSM
profile for nsjail, add that profile explicitly and record the audit decision;
do not use an unconfined profile as the default deployment mode.

The worker container still must not expose public ports. PostgreSQL and Redis
must remain on an internal network.

## Linux Host Check

Run on the worker host:

```bash
test -f /sys/fs/cgroup/cgroup.controllers
cat /sys/fs/cgroup/cgroup.controllers
```

Expected: a list of controllers including `memory` and `pids`.

## Acceptance Programs

Use `scripts/e2e-linux.sh` on a Linux worker host. It creates an A+B problem
with a low memory limit and submits AC/WA/CE/RE/TLE/MLE/OLE programs for
`cpp17`, `c11`, `python3` and `java17`.

Manual examples below are useful for debugging individual failures.

### Accepted

```cpp
#include <bits/stdc++.h>
using namespace std;
int main() {
  int a, b;
  cin >> a >> b;
  cout << a + b << "\n";
}
```

Expected:

- status `ACCEPTED`
- `time_ms > 0`
- `memory_kb > 0` on Linux cgroup v2

### Infinite Loop

```cpp
int main() {
  while (true) {}
}
```

Expected:

- status `TIME_LIMIT_EXCEEDED`
- no leftover nsjail or user process after the run

Check:

```bash
ps -ef | grep -E 'nsjail|/work/main' | grep -v grep
```

### Memory Limit

```cpp
#include <vector>
int main() {
  std::vector<int> v;
  while (true) v.resize(v.size() + 1000000);
}
```

Expected:

- status `MEMORY_LIMIT_EXCEEDED`
- `memory_kb` is non-zero and near the configured limit

### stdout Output Limit

```cpp
#include <iostream>
int main() {
  while (true) std::cout << "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx\n";
}
```

Expected:

- status `OUTPUT_LIMIT_EXCEEDED`
- `stdout.txt` stops at the configured output boundary

### stderr Output Limit

```cpp
#include <iostream>
int main() {
  while (true) std::cerr << "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx\n";
}
```

Expected:

- status `OUTPUT_LIMIT_EXCEEDED`
- `stderr.txt` stops at the configured output boundary

### Fork Bomb

```cpp
#include <unistd.h>
int main() {
  while (true) fork();
}
```

Expected:

- status `RUNTIME_ERROR` or `SYSTEM_ERROR`
- host remains healthy
- cgroup `pids.max` and nsjail `--rlimit_nproc` prevent process exhaustion

### Compile Output Limit

Generate code that emits a large compile error, or include thousands of invalid
tokens.

Expected:

- status `COMPILE_ERROR`
- compile log is bounded

## Build Verification

```powershell
powershell -NoProfile -File scripts/verify-static.ps1 -SkipDockerBuild
```

Full runtime resource-limit verification requires Linux cgroup v2, nsjail and
Docker daemon:

```bash
bash scripts/e2e-linux.sh
```
