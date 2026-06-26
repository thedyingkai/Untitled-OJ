#!/usr/bin/env bash
# 用途：执行 OJOS Linux 运行级 E2E 验收，覆盖 worker 注册、多 worker 并发、资源限制和提交状态。
# 运行环境：Linux，需 Docker daemon、cgroup v2、nsjail、curl、jq，并配置 OJOS_WORKER_TOKEN。
# 执行目录：从仓库根目录执行：OJOS_WORKER_TOKEN=<token> bash scripts/e2e-linux.sh。
# 依赖工具：bash、docker、curl、jq、可访问的 OJOS Control Plane。
# 失败处理：任一步失败立即退出；不得在未执行或失败时记录为通过。
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
API_BASE="${OJOS_API_BASE:-http://localhost:8080/api}"
ADMIN_USERNAME="${OJOS_ADMIN_USERNAME:-admin}"
ADMIN_PASSWORD="${OJOS_ADMIN_PASSWORD:-admin-password-change-me}"
USER_A="${OJOS_USER_A:-ojos_user_a}"
USER_B="${OJOS_USER_B:-ojos_user_b}"
USER_PASSWORD="${OJOS_USER_PASSWORD:-ojos-password-change-me}"
WORKER_TOKEN="${OJOS_WORKER_TOKEN:-}"

if [[ -z "$WORKER_TOKEN" ]]; then
  echo "OJOS_WORKER_TOKEN is required for worker registration checks" >&2
  exit 1
fi

require_cmd() {
  command -v "$1" >/dev/null 2>&1 || {
    echo "missing command: $1" >&2
    exit 1
  }
}

api() {
  local method="$1"
  local path="$2"
  local token="${3:-}"
  local body="${4:-}"
  local args=(-sS -X "$method" "${API_BASE}${path}" -H "Content-Type: application/json")
  if [[ -n "$token" ]]; then
    args+=(-H "Authorization: Bearer ${token}")
  fi
  if [[ -n "$body" ]]; then
    args+=(-d "$body")
  fi
  curl "${args[@]}"
}

json_value() {
  jq -r "$1"
}

login() {
  local username="$1"
  local password="$2"
  api POST /auth/login "" "{\"username\":\"${username}\",\"password\":\"${password}\"}" |
    tee /tmp/ojos-login.json |
    json_value '.data.token // .token // empty'
}

register_user() {
  local username="$1"
  local password="$2"
  api POST /auth/register "" "{\"username\":\"${username}\",\"password\":\"${password}\"}" >/tmp/ojos-register.json || true
}

submit_and_wait() {
  local token="$1"
  local problem_id="$2"
  local language="$3"
  local code_file="$4"
  local expected="$5"
  local code
  code="$(jq -Rs . <"$code_file")"
  local resp id status
  resp="$(api POST /judge/submissions "$token" "{\"problem_id\":${problem_id},\"language\":\"${language}\",\"code\":${code}}")"
  id="$(jq -r '.submission_id // .data.submission_id // empty' <<<"$resp")"
  if [[ -z "$id" ]]; then
    echo "submit failed: $resp" >&2
    exit 1
  fi
  for _ in $(seq 1 120); do
    status="$(api GET "/judge/submissions/${id}" "$token" | jq -r '.status // .data.status // empty')"
    if [[ "$status" != "PENDING" && "$status" != "JUDGING" && -n "$status" ]]; then
      break
    fi
    sleep 1
  done
  if [[ "$status" != "$expected" ]]; then
    echo "submission ${id} expected ${expected}, got ${status}" >&2
    api GET "/judge/submissions/${id}" "$token" >&2
    api GET "/judge/submissions/${id}/cases" "$token" >&2 || true
    exit 1
  fi
  echo "submission ${id} ${language} => ${status}"
}

write_sources() {
  local dir="$1"
  mkdir -p "$dir"/{cpp17,c11,python3,java17}

  cat >"$dir/cpp17/ac.cpp" <<'SRC'
#include <bits/stdc++.h>
using namespace std;
int main(){ long long a,b; if(cin>>a>>b) cout<<a+b<<"\n"; }
SRC
  cat >"$dir/cpp17/wa.cpp" <<'SRC'
#include <bits/stdc++.h>
using namespace std;
int main(){ long long a,b; if(cin>>a>>b) cout<<a-b<<"\n"; }
SRC
  cat >"$dir/cpp17/ce.cpp" <<'SRC'
int main( { return 0; }
SRC
  cat >"$dir/cpp17/re.cpp" <<'SRC'
int main(){ int *p=nullptr; return *p; }
SRC
  cat >"$dir/cpp17/tle.cpp" <<'SRC'
int main(){ while(true){} }
SRC
  cat >"$dir/cpp17/mle.cpp" <<'SRC'
#include <vector>
int main(){ std::vector<char> v; while(true) v.resize(v.size()+1024*1024, 1); }
SRC
  cat >"$dir/cpp17/ole.cpp" <<'SRC'
#include <iostream>
int main(){ while(true) std::cout << "0123456789012345678901234567890123456789\n"; }
SRC

  cat >"$dir/c11/ac.c" <<'SRC'
#include <stdio.h>
int main(){ long long a,b; if(scanf("%lld%lld",&a,&b)==2) printf("%lld\n",a+b); return 0; }
SRC
  cat >"$dir/c11/wa.c" <<'SRC'
#include <stdio.h>
int main(){ long long a,b; if(scanf("%lld%lld",&a,&b)==2) printf("%lld\n",a-b); return 0; }
SRC
  cat >"$dir/c11/ce.c" <<'SRC'
int main( { return 0; }
SRC
  cat >"$dir/c11/re.c" <<'SRC'
int main(){ int *p=0; return *p; }
SRC
  cat >"$dir/c11/tle.c" <<'SRC'
int main(){ for(;;){} }
SRC
  cat >"$dir/c11/mle.c" <<'SRC'
#include <stdlib.h>
int main(){ while(malloc(1024*1024)){} return 0; }
SRC
  cat >"$dir/c11/ole.c" <<'SRC'
#include <stdio.h>
int main(){ for(;;) puts("0123456789012345678901234567890123456789"); }
SRC

  cat >"$dir/python3/ac.py" <<'SRC'
a,b=map(int,input().split())
print(a+b)
SRC
  cat >"$dir/python3/wa.py" <<'SRC'
a,b=map(int,input().split())
print(a-b)
SRC
  cat >"$dir/python3/ce.py" <<'SRC'
if True print("bad")
SRC
  cat >"$dir/python3/re.py" <<'SRC'
raise RuntimeError("boom")
SRC
  cat >"$dir/python3/tle.py" <<'SRC'
while True:
    pass
SRC
  cat >"$dir/python3/mle.py" <<'SRC'
x=[]
while True:
    x.append(bytearray(1024*1024))
SRC
  cat >"$dir/python3/ole.py" <<'SRC'
while True:
    print("0123456789012345678901234567890123456789")
SRC

  cat >"$dir/java17/MainAC.java" <<'SRC'
import java.util.*;
public class Main { public static void main(String[] args){ Scanner sc=new Scanner(System.in); System.out.println(sc.nextLong()+sc.nextLong()); } }
SRC
  cat >"$dir/java17/MainWA.java" <<'SRC'
import java.util.*;
public class Main { public static void main(String[] args){ Scanner sc=new Scanner(System.in); System.out.println(sc.nextLong()-sc.nextLong()); } }
SRC
  cat >"$dir/java17/MainCE.java" <<'SRC'
public class Main { public static void main(String[] args) { broken } }
SRC
  cat >"$dir/java17/MainRE.java" <<'SRC'
public class Main { public static void main(String[] args) { throw new RuntimeException("boom"); } }
SRC
  cat >"$dir/java17/MainTLE.java" <<'SRC'
public class Main { public static void main(String[] args) { while(true){} } }
SRC
  cat >"$dir/java17/MainMLE.java" <<'SRC'
import java.util.*;
public class Main { public static void main(String[] args) { ArrayList<byte[]> x=new ArrayList<>(); while(true) x.add(new byte[1024*1024]); } }
SRC
  cat >"$dir/java17/MainOLE.java" <<'SRC'
public class Main { public static void main(String[] args) { while(true) System.out.println("0123456789012345678901234567890123456789"); } }
SRC
}

main() {
  require_cmd curl
  require_cmd jq
  require_cmd docker

  if [[ "${OJOS_SKIP_DOCKER:-0}" != "1" ]]; then
    (cd "$ROOT" && docker compose -f deploy/compose/docker-compose.yml up -d --build)
  fi

  echo "waiting for gateway..."
  for _ in $(seq 1 120); do
    if curl -fsS "${API_BASE%/api}/health" >/dev/null 2>&1; then break; fi
    sleep 1
  done

  register_user "$USER_A" "$USER_PASSWORD"
  register_user "$USER_B" "$USER_PASSWORD"
  ADMIN_TOKEN="${OJOS_ADMIN_TOKEN:-$(login "$ADMIN_USERNAME" "$ADMIN_PASSWORD")}"
  USER_TOKEN="$(login "$USER_A" "$USER_PASSWORD")"
  if [[ -z "$ADMIN_TOKEN" || -z "$USER_TOKEN" ]]; then
    echo "login failed. Set OJOS_ADMIN_TOKEN or create an admin first." >&2
    exit 1
  fi

  local create_resp problem_id
  create_resp="$(api POST /problem/problems "$ADMIN_TOKEN" '{"title":"E2E A+B","slug":"e2e-a-plus-b","statement":"Read a and b, print a+b.","visibility":"public","time_limit_ms":1000,"memory_limit_mb":64,"tags":"e2e"}')"
  problem_id="$(jq -r '.problem_id // .data.problem_id // empty' <<<"$create_resp")"
  if [[ -z "$problem_id" ]]; then
    problem_id="$(api GET "/problem/problems?keyword=e2e-a-plus-b" "$ADMIN_TOKEN" | jq -r '.problems[0].id // .data.problems[0].id // empty')"
  fi
  if [[ -z "$problem_id" ]]; then
    echo "create/find problem failed: $create_resp" >&2
    exit 1
  fi
  api POST "/problem/problems/${problem_id}/test-cases" "$ADMIN_TOKEN" '{"case_no":1,"input":"1 2\n","answer":"3\n","score":100,"sample":true,"time_limit_ms":1000,"memory_limit_mb":64}' >/dev/null
  api POST "/problem/problems/${problem_id}/package/validate" "$ADMIN_TOKEN" | jq .

  local srcdir
  srcdir="$(mktemp -d)"
  write_sources "$srcdir"

  if [[ "${OJOS_START_LOCAL_WORKERS:-1}" == "1" ]]; then
    docker compose -f "$ROOT/deploy/compose/docker-compose.yml" up -d --scale judge-worker=2 judge-worker
  fi

  local workers
  for _ in $(seq 1 60); do
    workers="$(api GET /judge/admin/workers "$ADMIN_TOKEN" | jq '.workers | length')"
    [[ "$workers" -ge 1 ]] && break
    sleep 1
  done
  echo "workers online in admin API: $workers"

  submit_and_wait "$USER_TOKEN" "$problem_id" cpp17 "$srcdir/cpp17/ac.cpp" ACCEPTED
  submit_and_wait "$USER_TOKEN" "$problem_id" cpp17 "$srcdir/cpp17/wa.cpp" WRONG_ANSWER
  submit_and_wait "$USER_TOKEN" "$problem_id" cpp17 "$srcdir/cpp17/ce.cpp" COMPILE_ERROR
  submit_and_wait "$USER_TOKEN" "$problem_id" cpp17 "$srcdir/cpp17/re.cpp" RUNTIME_ERROR
  submit_and_wait "$USER_TOKEN" "$problem_id" cpp17 "$srcdir/cpp17/tle.cpp" TIME_LIMIT_EXCEEDED
  submit_and_wait "$USER_TOKEN" "$problem_id" cpp17 "$srcdir/cpp17/mle.cpp" MEMORY_LIMIT_EXCEEDED
  submit_and_wait "$USER_TOKEN" "$problem_id" cpp17 "$srcdir/cpp17/ole.cpp" OUTPUT_LIMIT_EXCEEDED

  submit_and_wait "$USER_TOKEN" "$problem_id" c11 "$srcdir/c11/ac.c" ACCEPTED
  submit_and_wait "$USER_TOKEN" "$problem_id" c11 "$srcdir/c11/wa.c" WRONG_ANSWER
  submit_and_wait "$USER_TOKEN" "$problem_id" c11 "$srcdir/c11/ce.c" COMPILE_ERROR
  submit_and_wait "$USER_TOKEN" "$problem_id" c11 "$srcdir/c11/re.c" RUNTIME_ERROR
  submit_and_wait "$USER_TOKEN" "$problem_id" c11 "$srcdir/c11/tle.c" TIME_LIMIT_EXCEEDED
  submit_and_wait "$USER_TOKEN" "$problem_id" c11 "$srcdir/c11/mle.c" MEMORY_LIMIT_EXCEEDED
  submit_and_wait "$USER_TOKEN" "$problem_id" c11 "$srcdir/c11/ole.c" OUTPUT_LIMIT_EXCEEDED

  submit_and_wait "$USER_TOKEN" "$problem_id" python3 "$srcdir/python3/ac.py" ACCEPTED
  submit_and_wait "$USER_TOKEN" "$problem_id" python3 "$srcdir/python3/wa.py" WRONG_ANSWER
  submit_and_wait "$USER_TOKEN" "$problem_id" python3 "$srcdir/python3/ce.py" COMPILE_ERROR
  submit_and_wait "$USER_TOKEN" "$problem_id" python3 "$srcdir/python3/re.py" RUNTIME_ERROR
  submit_and_wait "$USER_TOKEN" "$problem_id" python3 "$srcdir/python3/tle.py" TIME_LIMIT_EXCEEDED
  submit_and_wait "$USER_TOKEN" "$problem_id" python3 "$srcdir/python3/mle.py" MEMORY_LIMIT_EXCEEDED
  submit_and_wait "$USER_TOKEN" "$problem_id" python3 "$srcdir/python3/ole.py" OUTPUT_LIMIT_EXCEEDED

  submit_and_wait "$USER_TOKEN" "$problem_id" java17 "$srcdir/java17/MainAC.java" ACCEPTED
  submit_and_wait "$USER_TOKEN" "$problem_id" java17 "$srcdir/java17/MainWA.java" WRONG_ANSWER
  submit_and_wait "$USER_TOKEN" "$problem_id" java17 "$srcdir/java17/MainCE.java" COMPILE_ERROR
  submit_and_wait "$USER_TOKEN" "$problem_id" java17 "$srcdir/java17/MainRE.java" RUNTIME_ERROR
  submit_and_wait "$USER_TOKEN" "$problem_id" java17 "$srcdir/java17/MainTLE.java" TIME_LIMIT_EXCEEDED
  submit_and_wait "$USER_TOKEN" "$problem_id" java17 "$srcdir/java17/MainMLE.java" MEMORY_LIMIT_EXCEEDED
  submit_and_wait "$USER_TOKEN" "$problem_id" java17 "$srcdir/java17/MainOLE.java" OUTPUT_LIMIT_EXCEEDED

  api GET /judge/admin/queue "$ADMIN_TOKEN" | jq .
  api GET /admin/health "$ADMIN_TOKEN" | jq .

  echo "E2E Linux acceptance completed."
}

main "$@"
