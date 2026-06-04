# Judge Sandbox 文档

## 一、模块定位

Judge Sandbox 是 OJOS Judge Worker 中负责隔离执行用户代码的安全边界。

当前 OJOS 使用：

```text
nsjail
```

作为基础沙箱工具。

Sandbox 的目标不是“让程序能跑”，而是：

```text
让不可信用户程序在受限环境中运行
```

它需要保证：

```text
用户程序不能读取题目答案
用户程序不能覆盖题目输入 / 答案
用户程序不能访问 worker 完整文件系统
用户程序不能以 root 身份运行
用户程序不能访问外部网络
用户程序不能影响其他提交
用户程序不能长期占用 CPU
用户程序不能无限创建进程
用户程序不能无限消耗内存
用户程序不能制造无限输出
```

当前 Sandbox 已经完成基础隔离，但仍不是最终生产级沙箱。

当前已完成：

```text
nsjail 编译隔离
nsjail 运行隔离
非 root 用户运行
独立 mount namespace
独立 pid namespace
独立 ipc namespace
独立 uts namespace
独立 net namespace
用户程序只看到 /work
用户程序看不到 /data/ojos/problems
用户程序无法读取 *.ans
每个测试点独立运行目录
stdin / stdout / stderr 通过 /work 文件重定向
基础时间限制
基础地址空间限制
基础进程数限制
基础文件描述符限制
```

当前仍需完善：

```text
cgroup v2 memory peak 统计
真实 memory_kb 写回
输出大小限制
stderr 大小限制
文件大小限制
更严格的系统调用限制
更细粒度的语言沙箱策略
并发评测隔离策略
```

---

## 二、为什么使用 nsjail

Judge Worker 会执行用户提交的代码，而用户代码默认是不可信的。

如果直接在 worker 容器内执行用户程序，恶意代码可能：

```text
读取题目答案
读取其他提交文件
读取 worker 配置
修改测试数据
创建大量进程
消耗大量内存
访问网络
影响 Redis / PostgreSQL
阻塞 worker
破坏评测目录
```

因此必须引入沙箱。

当前选择 `nsjail` 的原因：

```text
支持 Linux namespace
支持 chroot
支持 uid / gid 降权
支持只读 bind mount
支持 tmpfs mount
支持时间限制
支持 rlimit
支持禁用网络 namespace
适合在 Docker 容器内作为二级隔离
```

当前架构不是：

```text
Docker 容器 = 唯一沙箱
```

而是：

```text
Docker 容器
  ↓
judge-worker
  ↓
nsjail
  ↓
用户程序
```

Docker 负责 worker 的部署隔离。

nsjail 负责每次编译 / 每个测试点运行的用户程序隔离。

---

## 三、当前安全目标

当前 Sandbox 需要满足的基础目标：

```text
1. 用户程序不能看到题目包目录
2. 用户程序不能读取 answer 文件
3. 用户程序不能覆盖 input / answer 文件
4. 用户程序不能看到 /data/ojos/problems
5. 用户程序只能在 /work 中读写
6. 用户程序使用 uid/gid 10001 运行
7. 每个测试点使用独立 /work
8. 每个测试点运行结束后结果落盘
9. 编译和运行都经过 nsjail
10. 不使用 Docker privileged
```

当前已验证：

```text
jail 内 /data/ojos/problems 不存在
jail 内 uid=10001 gid=10001
/work 可写
用户程序尝试读取 /data/ojos/problems/.../*.ans 会失败
```

---

## 四、Docker 层配置

`judge-worker` 容器不应使用：

```yaml
privileged: true
```

当前应使用最小 capability 方式支持 nsjail：

```yaml
cap_add:
  - SYS_ADMIN
  - SYS_CHROOT
  - SETUID
  - SETGID
  - NET_ADMIN
```

推荐 Compose 片段：

```yaml
judge-worker:
  build:
    context: ../../services
    dockerfile: judge-worker/Dockerfile
  container_name: ojos-judge-worker
  depends_on:
    postgres:
      condition: service_healthy
    redis:
      condition: service_started
  environment:
    DATABASE_URL: postgres://postgres:password@postgres:5432/ojos?sslmode=disable
    REDIS_URL: redis://ojos-redis:6379/0
    LANGUAGES_CONFIG: config/languages.yaml
  volumes:
    - ../../storage:/data/ojos
  cap_add:
    - SYS_ADMIN
    - SYS_CHROOT
    - SETUID
    - SETGID
    - NET_ADMIN
```

说明：

```text
../../storage:/data/ojos
```

让 worker 容器可以访问：

```text
/data/ojos/problems
/data/ojos/submissions
```

但 nsjail 内的用户程序不会挂载 `/data/ojos/problems`。

---

## 五、Dockerfile 要求

Judge Worker 镜像中需要提供：

```text
nsjail
bash
coreutils
gcc
g++
python3
openjdk-17-jdk
```

还需要准备 jail 根目录：

```text
/jail/root
/jail/root/dev
```

推荐 Dockerfile 关键点：

```dockerfile
RUN mkdir -p /jail/root/dev /data/ojos/problems /data/ojos/submissions
```

如果需要在 jail 内挂载：

```text
/dev/null
/dev/zero
/dev/urandom
```

则 `/jail/root/dev` 必须存在。

否则 nsjail bind mount 设备文件时可能失败。

---

## 六、当前 nsjail 基础参数

当前 nsjail 参数类似：

```text
nsjail
  --mode o
  --user 10001
  --group 10001
  --disable_clone_newuser
  --time_limit <sec>
  --rlimit_as <memory_mb>
  --rlimit_nofile 64
  --rlimit_nproc 64
  --cwd /work
  --chroot /jail/root
  --bindmount_ro /bin:/bin
  --bindmount_ro /lib:/lib
  --bindmount_ro /lib64:/lib64
  --bindmount_ro /usr:/usr
  --bindmount_ro /etc/alternatives:/etc/alternatives
  --bindmount_ro /dev/null:/dev/null
  --bindmount_ro /dev/zero:/dev/zero
  --bindmount_ro /dev/urandom:/dev/urandom
  --bindmount <case_or_build_dir>:/work
  --tmpfsmount /tmp
  --
  /bin/bash -lc "<command>"
```

参数含义：

| 参数                        | 含义                      |
| ------------------------- | ----------------------- |
| `--mode o`                | 单次运行模式                  |
| `--user 10001`            | jail 内用户 UID            |
| `--group 10001`           | jail 内用户 GID            |
| `--disable_clone_newuser` | 禁用 user namespace clone |
| `--time_limit`            | 墙钟时间限制                  |
| `--rlimit_as`             | 地址空间限制                  |
| `--rlimit_nofile`         | 文件描述符数量限制               |
| `--rlimit_nproc`          | 进程数量限制                  |
| `--cwd /work`             | jail 内工作目录              |
| `--chroot /jail/root`     | jail 根目录                |
| `--bindmount_ro`          | 只读挂载                    |
| `--bindmount`             | 可写挂载                    |
| `--tmpfsmount /tmp`       | 提供临时 tmpfs              |
| `--`                      | nsjail 参数结束，后面是真实执行命令   |

注意：

```text
所有 nsjail 参数必须放在 -- 前面
-- 后面才是真正执行的命令
```

错误写法：

```text
nsjail ... -- /bin/bash -lc "<command>" --user 10001
```

这种情况下：

```text
--user 10001
```

已经不是 nsjail 参数，而是 bash 参数。

正确写法：

```text
nsjail --user 10001 ... -- /bin/bash -lc "<command>"
```

---

## 七、文件系统隔离模型

当前 worker 容器可以看到：

```text
/data/ojos/problems
/data/ojos/submissions
```

但用户程序不应该看到：

```text
/data/ojos/problems
```

用户程序只应该看到：

```text
/work
/tmp
/bin
/lib
/lib64
/usr
/etc/alternatives
/dev/null
/dev/zero
/dev/urandom
```

其中：

```text
/work
```

是每次编译或每个测试点的工作目录。

编译阶段：

```text
/work = storage/submissions/{submission_id}/build
```

运行阶段：

```text
/work = storage/submissions/{submission_id}/cases/{case_no:03}
```

题目输入和答案的处理方式：

```text
worker 在 jail 外读取题目 input
worker 将 input 复制到 case_dir/stdin.txt
worker 不把 answer 挂进 jail
用户程序只读取 /work/stdin.txt
用户程序写 /work/stdout.txt 和 /work/stderr.txt
worker 在 jail 外读取 answer
worker 在 jail 外执行 checker
```

也就是说：

```text
answer 文件永远不进入用户程序可见的 /work
```

这是防止读答案的关键。

---

## 八、编译阶段沙箱

编译阶段使用：

```text
storage/submissions/{submission_id}/build
```

作为 `/work`。

典型结构：

```text
storage/submissions/{submission_id}/build/

├── main.cpp
├── main
├── compile.log
├── compile.stdout.log
└── compile.stderr.log
```

C++ 编译命令示例：

```text
/usr/bin/g++ -std=c++17 -O2 -pipe -B/usr/bin/ /work/main.cpp -o /work/main
```

编译日志应在 jail 内重定向：

```text
/work/compile.stdout.log
/work/compile.stderr.log
```

最终合并为：

```text
compile.log
```

不要依赖父进程 FD 捕获编译日志。

原因：

```text
nsjail + Docker + Windows bind mount 场景下，父进程 FD 捕获可能不稳定
```

如果编译失败，结果应为：

```text
status = COMPILE_ERROR
cases = []
message = compile.log 摘要
```

---

## 九、运行阶段沙箱

每个测试点使用独立目录：

```text
storage/submissions/{submission_id}/cases/{case_no:03}
```

例如：

```text
storage/submissions/20/cases/001
```

目录结构：

```text
cases/001/

├── main
├── stdin.txt
├── stdout.txt
├── stderr.txt
└── checker.log
```

运行命令应在 jail 内显式重定向：

```text
/work/main < /work/stdin.txt > /work/stdout.txt 2> /work/stderr.txt
```

不要依赖父进程 FD 传递 stdin / stdout / stderr。

原因：

```text
Windows bind mount + nsjail 下，父进程 FD 传递可能导致 stdout.txt 为空
```

每个 case 运行前应删除旧文件：

```text
stdout.txt
stderr.txt
checker.log
```

原因：

```text
旧文件可能属于 root 且权限为 0644
用户程序 uid=10001 无法截断
bash 会在重定向阶段直接失败
```

正确流程：

```text
创建 case_dir
删除旧 stdout.txt / stderr.txt / checker.log
写入 stdin.txt
复制可执行文件到 case_dir
设置可执行权限
nsjail 运行
读取 stdout.txt / stderr.txt
checker 比较
写 checker.log
```

---

## 十、用户身份与权限

用户程序在 jail 中应以：

```text
uid = 10001
gid = 10001
```

运行。

验证命令：

```bash
nsjail --mode o \
  --user 10001 \
  --group 10001 \
  --disable_clone_newuser \
  --time_limit 2 \
  --cwd /work \
  --chroot /jail/root \
  --bindmount_ro /bin:/bin \
  --bindmount_ro /lib:/lib \
  --bindmount_ro /lib64:/lib64 \
  --bindmount_ro /usr:/usr \
  --bindmount_ro /etc/alternatives:/etc/alternatives \
  --bindmount_ro /dev/null:/dev/null \
  --bindmount_ro /dev/zero:/dev/zero \
  --bindmount_ro /dev/urandom:/dev/urandom \
  --bindmount /tmp/jailtest:/work \
  --tmpfsmount /tmp \
  -- /bin/bash -lc 'id && pwd && touch /work/write-test'
```

预期：

```text
uid=10001 gid=10001 groups=10001
/work
write-test 创建成功
```

如果日志中出现：

```text
Process will be UID/EUID=0 in the global user namespace
```

需要检查是否忘记：

```text
--disable_clone_newuser
```

以及 `--user / --group` 是否放在 `--` 前面。

---

## 十一、网络隔离

当前 nsjail 默认启用新的 network namespace：

```text
clone_newnet:true
```

这意味着用户程序不应直接访问宿主网络。

当前不应给用户程序挂载：

```text
Docker socket
PostgreSQL socket
Redis socket
宿主网络命名空间
```

后续如需支持交互题或通信题，也不应直接开放外网，而应通过 Runner Core 管理进程间通信。

---

## 十二、题目答案保护

当前核心安全目标之一是：

```text
用户程序不能读取 answer 文件
```

当前设计：

```text
题目包位于 /data/ojos/problems
worker 可读
jail 不挂载 /data/ojos/problems
worker 将 input 复制到 /work/stdin.txt
worker 不复制 answer 到 /work
checker 在 jail 外读取 answer
```

验证程序：

```cpp
#include <bits/stdc++.h>
using namespace std;

int main() {
    ifstream fin("/data/ojos/problems/2-a-plus-b/tests/001.ans");
    if (fin.good()) {
        string s;
        getline(fin, s);
        cout << s << '\n';
    } else {
        cout << "NO_ANSWER_VISIBLE\n";
    }
    return 0;
}
```

预期：

```text
stdout = NO_ANSWER_VISIBLE
status = WRONG_ANSWER
```

重点不是该程序 AC，而是：

```text
它不能读到 answer
```

---

## 十三、防止覆写输入和答案

用户程序不应能覆写：

```text
题目 input
题目 answer
```

当前设计中，题目 input / answer 文件位于：

```text
/data/ojos/problems/{id}-{slug}/tests/
```

而 jail 内不可见 `/data/ojos/problems`。

用户程序只能写：

```text
/work
/tmp
```

其中 `/work/stdin.txt` 是复制出来的输入副本。

即使用户程序覆写：

```text
/work/stdin.txt
```

也不会影响题目包中的原始输入。

答案文件不被复制到 `/work`，因此用户程序没有机会覆写答案。

---

## 十四、时间限制

当前时间限制通过 nsjail：

```text
--time_limit <sec>
```

实现。

注意：

```text
--time_limit 使用秒
题目和语言配置中通常使用毫秒
```

因此 worker 需要做转换，例如：

```text
time_limit_sec = ceil(time_ms / 1000)
```

对于：

```text
time_ms = 1000
```

应转换为：

```text
1
```

对于：

```text
time_ms = 1500
```

建议转换为：

```text
2
```

当前 TLE 应映射为：

```text
TIME_LIMIT_EXCEEDED
```

并写入：

```text
result.json
checker.log
submissions.message
```

---

## 十五、内存限制

当前内存限制主要依赖：

```text
--rlimit_as <memory_mb>
```

它限制进程地址空间。

当前限制：

```text
memory_kb 暂未采集
不能真实显示峰值内存
```

后续应接入：

```text
cgroup v2
```

用于统计：

```text
memory.current
memory.peak
memory.max
```

最终写入：

```text
case.memory_kb
submission.memory_kb
```

当前不要伪造 memory_kb。

---

## 十六、进程数限制

当前使用：

```text
--rlimit_nproc 64
```

限制进程数量。

目的：

```text
防止 fork bomb
限制用户程序创建大量子进程
```

后续可以根据语言调整。

例如：

```text
C++ / C: 较小
Java: 略大
Python: 较小
交互题 / 通信题: 由 Runner Core 单独控制
```

---

## 十七、文件描述符限制

当前使用：

```text
--rlimit_nofile 64
```

目的：

```text
防止用户程序打开大量文件
减少资源滥用
```

后续可以按语言和题型调整。

---

## 十八、输出大小限制

当前输出大小限制尚未完善。

风险：

```text
用户程序无限输出
stdout.txt 巨大
stderr.txt 巨大
磁盘被写满
checker 读取巨大文件耗时
result.json 过大
```

后续应增加：

```text
stdout size limit
stderr size limit
checker_log size limit
compile_log size limit
```

实现方式可以是：

```text
运行后检查文件大小，超限判 OUTPUT_LIMIT_EXCEEDED
使用 rlimit_fsize，但要避免影响编译产物
用 wrapper 控制 stdout/stderr
```

注意：

```text
不要在编译阶段随意使用很小的 rlimit_fsize
```

否则 g++ 可能无法生成中间文件或可执行文件。

---

## 十九、文件大小限制

之前曾尝试：

```text
--rlimit_fsize 64
```

这会导致 C++ 编译失败，因为 g++ 需要写目标文件和可执行文件。

因此当前不建议在编译阶段使用过小的：

```text
--rlimit_fsize
```

后续应区分：

```text
编译阶段文件限制
运行阶段输出限制
```

而不是一个限制套所有阶段。

---

## 二十、C/C++ 编译注意事项

C/C++ 编译建议使用绝对命令：

```text
/usr/bin/g++
/usr/bin/gcc
```

并在参数中加入：

```text
-B/usr/bin/
```

原因是 g++ 内部需要调用：

```text
ld
collect2
as
```

如果 PATH 或工具链查找路径不完整，可能出现：

```text
collect2: fatal error: cannot find 'ld'
```

推荐 `cpp17` 配置：

```yaml
cpp17:
  source_file: main.cpp
  exe_file: main
  compile:
    enabled: true
    command: /usr/bin/g++
    args:
      - "-std=c++17"
      - "-O2"
      - "-pipe"
      - "-B/usr/bin/"
      - "{source}"
      - "-o"
      - "{exe}"
    timeout_ms: 10000
    memory_mb: 2048
  run:
    command: "{exe}"
    args: []
```

同时 shell 中应设置 PATH：

```text
export PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
```

---

## 二十一、Java 注意事项

Java 需要关注：

```text
javac 编译目录
java -cp {workdir} Main
内存参数
进程数
启动耗时
JVM 临时文件
```

Java 运行命令建议后续加入：

```text
-Xms
-Xmx
```

例如：

```text
/usr/bin/java -Xmx256m -cp /work Main
```

当前如果只靠 `--rlimit_as`，可能导致 JVM 启动失败或行为不稳定。

Java 需要单独验收。

---

## 二十二、Python 注意事项

Python 运行命令：

```text
/usr/bin/python3 /work/main.py
```

Python 风险：

```text
import os 访问文件
创建子进程
无限递归
无限输出
读取 /proc
```

当前 jail 中存在 `/proc`，后续应继续评估是否需要限制或最小化 `/proc` 暴露。

Python 也需要单独验收：

```text
AC
WA
RE
TLE
防读取 answer
```

---

## 二十三、手动 nsjail 调试命令

进入 worker 容器：

```powershell
docker exec -it ojos-judge-worker bash
```

容器内执行：

```bash
cd /data/ojos/submissions/20/build

nsjail --mode o \
  --user 10001 \
  --group 10001 \
  --disable_clone_newuser \
  --time_limit 10 \
  --rlimit_as 2048 \
  --rlimit_nofile 64 \
  --rlimit_nproc 64 \
  --cwd /work \
  --chroot /jail/root \
  --bindmount_ro /bin:/bin \
  --bindmount_ro /lib:/lib \
  --bindmount_ro /lib64:/lib64 \
  --bindmount_ro /usr:/usr \
  --bindmount_ro /etc/alternatives:/etc/alternatives \
  --bindmount_ro /dev/null:/dev/null \
  --bindmount_ro /dev/zero:/dev/zero \
  --bindmount_ro /dev/urandom:/dev/urandom \
  --bindmount /data/ojos/submissions/20/build:/work \
  --tmpfsmount /tmp \
  -- /bin/bash -lc 'set -x; id; pwd; ls -lah /work; /usr/bin/g++ --version; /usr/bin/g++ -std=c++17 -O2 -pipe -B/usr/bin/ /work/main.cpp -o /work/main; echo rc=$?; ls -lah /work'
```

用于排查：

```text
g++ 是否可用
ld 是否可用
/work 是否可写
uid/gid 是否正确
编译产物是否生成
```

---

## 二十四、常见错误

### 24.1 `collect2: fatal error: cannot find 'ld'`

原因：

```text
g++ 找不到链接器 ld
```

修法：

```text
使用 /usr/bin/g++
加入 -B/usr/bin/
设置 PATH
确保 /usr 挂载进 jail
```

---

### 24.2 `process exited with code 127`

通常表示：

```text
command not found
```

常见原因：

```text
run.command = "{exe}" 没有替换成 /work/main
命令使用相对路径但 PATH 不完整
可执行文件没复制到 case_dir
```

修法：

```text
command 和 args 都执行占位符替换
run.command 应变成 /work/main
检查 case_dir 中是否有 main
```

---

### 24.3 `process exited with code 126`

通常表示：

```text
permission denied
```

常见原因：

```text
可执行文件没有执行权限
```

修法：

```text
复制 executable 到 case_dir 后 chmod 755
```

---

### 24.4 stdout 为空但程序应输出

常见原因：

```text
stdin/stdout FD 传递不稳定
重定向文件权限不对
stdout.txt 是旧 root 文件，uid=10001 无法截断
```

修法：

```text
使用 jail 内文件重定向
运行前删除 stdout.txt / stderr.txt
让 uid=10001 自己创建新文件
```

---

### 24.5 `load cases.yaml failed`

常见原因：

```text
problem.yaml 路径错误
tests.cases 拼接错误
cases.yaml 不存在
旧格式 no: 0
```

修法：

```text
tests.cases 相对 package_dir
case.input / case.answer 相对 tests.root
不要拼成 tests/tests/cases.yaml
使用 case_no: 1
```

---

### 24.6 `Operation not permitted` when ifaceUp lo

如果 nsjail 日志出现：

```text
ifaceUp(): ioctl(iface='lo', SIOCSIFFLAGS, IFF_UP|IFF_RUNNING): Operation not permitted
```

可能与：

```text
network namespace
NET_ADMIN capability
disable_clone_newuser
```

有关。

当前 Compose 需要：

```text
NET_ADMIN
```

如果不需要 loopback，也可以后续调整 nsjail 网络参数。

---

## 二十五、安全验收用例

### 25.1 确认 jail 内看不到 problems

```bash
nsjail --mode o \
  --user 10001 \
  --group 10001 \
  --disable_clone_newuser \
  --time_limit 2 \
  --cwd /work \
  --chroot /jail/root \
  --bindmount_ro /bin:/bin \
  --bindmount_ro /lib:/lib \
  --bindmount_ro /lib64:/lib64 \
  --bindmount_ro /usr:/usr \
  --bindmount_ro /etc/alternatives:/etc/alternatives \
  --bindmount /tmp/jailtest:/work \
  --tmpfsmount /tmp \
  -- /bin/bash -lc 'ls /data/ojos/problems || echo no-problems-visible'
```

预期：

```text
no-problems-visible
```

---

### 25.2 确认 uid/gid

```bash
nsjail ... -- /bin/bash -lc 'id'
```

预期：

```text
uid=10001 gid=10001 groups=10001
```

---

### 25.3 确认 /work 可写

```bash
nsjail ... -- /bin/bash -lc 'touch /work/write-test && echo write-ok'
```

预期：

```text
write-ok
```

---

### 25.4 确认不能读取 answer

提交以下程序：

```cpp
#include <bits/stdc++.h>
using namespace std;

int main() {
    ifstream fin("/data/ojos/problems/2-a-plus-b/tests/001.ans");
    if (fin.good()) {
        string s;
        getline(fin, s);
        cout << s << '\n';
    } else {
        cout << "NO_ANSWER_VISIBLE\n";
    }
    return 0;
}
```

预期：

```text
stdout.txt = NO_ANSWER_VISIBLE
status = WRONG_ANSWER
```

重点是不能读到答案。

---

## 二十六、当前已知限制

当前 Sandbox 仍有以下限制：

```text
memory_kb 暂未统计
输出大小限制尚未实现
stderr 大小限制尚未实现
compile.log 大小限制尚未实现
系统调用限制尚未细化
多语言资源限制尚未精细化
Java / Python 沙箱策略仍需单独验证
交互题 / 通信题需要新的 Runner Core
```

这些限制不影响当前传统题主链路验收，但在开放给真实不可信用户前必须继续加强。

---

## 二十七、后续计划

Sandbox 后续建议按以下顺序推进：

```text
1. cgroup v2 memory peak 统计
2. stdout / stderr / compile log 大小限制
3. 按语言区分 memory / process / file limit
4. XAUTOCLAIM 处理卡住任务
5. JUDGING 超时恢复
6. Runner Core 抽象
7. Special Judge 隔离执行策略
8. 交互题双进程沙箱
9. 通信题多进程沙箱
10. 系统调用策略
```

当前不要急着把交互题、通信题、启发式题全部塞进现有 `sandbox.rs`。

应先抽象：

```text
Runner Core
Sandbox Provider
RunResult
CompileResult
ResourceLimit
```

再扩展复杂题型。

---

## 二十八、当前结论

当前 OJOS Sandbox 已经完成从：

```text
容器内裸跑用户程序
```

到：

```text
nsjail 基础隔离执行
```

的关键升级。

当前已经实现：

```text
编译隔离
运行隔离
非 root 执行
/work 限定
题目包不可见
答案文件不可见
每 case 独立目录
文件重定向
基础时间限制
基础地址空间限制
```

当前已经可以支撑传统题开发阶段验收。

但它仍然不是完整生产级沙箱。

下一阶段必须补齐：

```text
真实内存统计
输出限制
系统调用限制
多语言沙箱策略
Runner Core 抽象
```

完成这些后，OJOS 才能更安全地支持：

```text
公网评测
正式比赛
多语言提交
交互题
通信题
提交答案题
Special Judge
复杂赛制
```
