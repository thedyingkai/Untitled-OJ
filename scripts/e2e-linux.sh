#!/usr/bin/env bash
# Purpose:
#   Run OJOS Linux Judge Runtime acceptance against a real Docker Control Plane.
# Preconditions:
#   Linux host/WSL2 with Docker, cgroup v2, nsjail-capable judge-worker image,
#   curl, jq, and a project-root .env containing OJOS_WORKER_TOKEN.
# Usage:
#   OJOS_ADMIN_USERNAME=admin1 OJOS_ADMIN_PASSWORD=admin123 \
#   OJOS_USER_A=user1 OJOS_USER_PASSWORD=user123 \
#   bash scripts/e2e-linux.sh
# Output:
#   Reports/logs/scratch data are written only under .tmp/agent/.
# Failure:
#   Any failed assertion exits non-zero. Do not record unexecuted checks as passed.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
COMPOSE_FILE="${OJOS_COMPOSE_FILE:-deploy/compose/docker-compose.yml}"
ENV_FILE="${OJOS_ENV_FILE:-.env}"
API_BASE="${OJOS_API_BASE:-http://localhost:8080/api}"
PUBLIC_BASE="${API_BASE%/api}"
ADMIN_USERNAME="${OJOS_ADMIN_USERNAME:-admin1}"
ADMIN_PASSWORD="${OJOS_ADMIN_PASSWORD:-admin123}"
USER_A="${OJOS_USER_A:-user1}"
USER_B="${OJOS_USER_B:-linux_user_b}"
USER_PASSWORD="${OJOS_USER_PASSWORD:-user123}"

REPORT_DIR="$ROOT/.tmp/agent/reports/linux-runtime"
LOG_DIR="$ROOT/.tmp/agent/logs/linux-runtime"
SCRATCH_DIR="$ROOT/.tmp/agent/scratch/linux-runtime"
MATRIX_TSV="$REPORT_DIR/status-matrix.tsv"
SUMMARY_JSON="$REPORT_DIR/summary.json"
CRASH_REPORT="$REPORT_DIR/worker-crash-recovery.md"

mkdir -p "$REPORT_DIR" "$LOG_DIR" "$SCRATCH_DIR"

cd "$ROOT"

read_env_value() {
  local key="$1"
  local line value
  if [[ ! -f "$ENV_FILE" ]]; then
    return 0
  fi
  line="$(grep -E "^[[:space:]]*${key}=" "$ENV_FILE" | tail -n 1 || true)"
  if [[ -z "$line" ]]; then
    return 0
  fi
  value="${line#*=}"
  value="${value%$'\r'}"
  value="${value#"${value%%[![:space:]]*}"}"
  value="${value%"${value##*[![:space:]]}"}"
  if [[ "$value" == \"*\" && "$value" == *\" ]]; then
    value="${value:1:${#value}-2}"
  elif [[ "$value" == \'*\' && "$value" == *\' ]]; then
    value="${value:1:${#value}-2}"
  fi
  printf '%s' "$value"
}

WORKER_TOKEN="${OJOS_WORKER_TOKEN:-$(read_env_value OJOS_WORKER_TOKEN)}"
POSTGRES_DB="${POSTGRES_DB:-$(read_env_value POSTGRES_DB)}"
POSTGRES_USER="${POSTGRES_USER:-$(read_env_value POSTGRES_USER)}"
POSTGRES_DB="${POSTGRES_DB:-ojos}"
POSTGRES_USER="${POSTGRES_USER:-postgres}"

if [[ -z "$WORKER_TOKEN" ]]; then
  echo "OJOS_WORKER_TOKEN is required" >&2
  exit 1
fi

ADMIN_TOKEN=""
USER_TOKEN=""
USER_B_TOKEN=""
PROBLEM_ID=""
LONG_PROBLEM_ID=""
PATH_LEAKS=0
PERMISSION_FAILURES=0
MEMORY_NONZERO=0
MATRIX_TOTAL=0
MATRIX_FAILED=0
DISTINCT_MATRIX_WORKERS=0
DOUBLE_WORKER_OK=false
CRASH_RECOVERY_OK=false
STALE_LEASE_REJECTED=false
REDIS_SIGNAL_OK=false
ADMIN_HEALTH_STATUS=""
ADMIN_HEALTH_JUDGE_STATUS=""

compose() {
  docker compose --env-file "$ENV_FILE" -f "$COMPOSE_FILE" "$@"
}

require_cmd() {
  command -v "$1" >/dev/null 2>&1 || {
    echo "missing command: $1" >&2
    exit 1
  }
}

die() {
  echo "ERROR: $*" >&2
  exit 1
}

urlencode() {
  jq -rn --arg v "$1" '$v|@uri'
}

json_body() {
  jq -nc "$@"
}

api_raw() {
  local method="$1"
  local path="$2"
  local token="${3:-}"
  local body="${4:-}"
  local out="${5:-}"
  local url
  if [[ "$path" == http* ]]; then
    url="$path"
  else
    url="${API_BASE}${path}"
  fi

  local args=(-sS -w $'\n%{http_code}' -X "$method" "$url" -H "Content-Type: application/json")
  if [[ -n "$token" ]]; then
    args+=(-H "Authorization: Bearer ${token}")
  fi
  if [[ -n "$body" ]]; then
    args+=(-d "$body")
  fi

  local response status text
  response="$(curl "${args[@]}")"
  status="${response##*$'\n'}"
  text="${response%$'\n'*}"
  if [[ -n "$out" ]]; then
    printf '%s' "$text" >"$out"
  fi
  printf '%s\t%s\n' "$status" "$text"
}

api() {
  local method="$1"
  local path="$2"
  local token="${3:-}"
  local body="${4:-}"
  local expected="${5:-200}"
  local out="${6:-}"

  local result status text
  result="$(api_raw "$method" "$path" "$token" "$body" "$out")"
  status="${result%%$'\t'*}"
  text="${result#*$'\t'}"
  if ! grep -Eq "(^|,)$status(,|$)" <<<"$expected"; then
    echo "API $method $path expected $expected got $status: $text" >&2
    return 1
  fi
  printf '%s' "$text"
}

api_status() {
  local method="$1"
  local path="$2"
  local token="${3:-}"
  local body="${4:-}"
  local result
  result="$(api_raw "$method" "$path" "$token" "$body")"
  printf '%s' "${result%%$'\t'*}"
}

worker_api() {
  local method="$1"
  local path="$2"
  local body="${3:-}"
  local expected="${4:-200}"
  local response status text
  response="$(curl -sS -w $'\n%{http_code}' -X "$method" "${API_BASE}${path}" \
    -H "Content-Type: application/json" \
    -H "X-OJOS-Worker-Token: ${WORKER_TOKEN}" \
    -d "$body")"
  status="${response##*$'\n'}"
  text="${response%$'\n'*}"
  if ! grep -Eq "(^|,)$status(,|$)" <<<"$expected"; then
    echo "Worker API $method $path expected $expected got $status: $text" >&2
    return 1
  fi
  printf '%s' "$text"
}

worker_status() {
  local method="$1"
  local path="$2"
  local body="${3:-}"
  local response
  response="$(curl -sS -w $'\n%{http_code}' -X "$method" "${API_BASE}${path}" \
    -H "Content-Type: application/json" \
    -H "X-OJOS-Worker-Token: ${WORKER_TOKEN}" \
    -d "$body")"
  printf '%s' "${response##*$'\n'}"
}

wait_for_gateway() {
  echo "waiting for gateway..."
  local ok=false
  for _ in $(seq 1 180); do
    if curl -fsS "${PUBLIC_BASE}/health" >/dev/null 2>&1; then
      ok=true
      break
    fi
    sleep 1
  done
  [[ "$ok" == true ]] || die "gateway did not become healthy"
}

apply_migrations() {
  echo "applying migrations..."
  local schema_ready
  schema_ready="$(printf "SELECT CASE WHEN to_regclass('public.module_sets') IS NOT NULL AND to_regclass('public.judge_tasks') IS NOT NULL THEN 'yes' ELSE 'no' END;\n" |
    compose exec -T postgres psql -tA -v ON_ERROR_STOP=1 -U "$POSTGRES_USER" -d "$POSTGRES_DB" 2>/dev/null || true)"
  if [[ "$schema_ready" == "yes" ]]; then
    echo "schema already contains module_sets and judge_tasks; skip migrations" >>"$LOG_DIR/migrations.log"
    return 0
  fi
  for file in deploy/migrations/*.up.sql; do
    echo "apply $file" >>"$LOG_DIR/migrations.log"
    compose exec -T postgres psql -v ON_ERROR_STOP=1 -U "$POSTGRES_USER" -d "$POSTGRES_DB" <"$file" >>"$LOG_DIR/migrations.log" 2>&1
  done
}

ensure_admin_role() {
  local username="$1"
  local sql
  sql="$(cat <<SQL
INSERT INTO user_roles(user_id, role_id)
SELECT u.id, r.id
FROM users u
JOIN roles r ON r.name = 'super_admin'
WHERE u.username = '${username//\'/\'\'}'
ON CONFLICT DO NOTHING;
SQL
)"
  printf '%s\n' "$sql" | compose exec -T postgres psql -v ON_ERROR_STOP=1 -U "$POSTGRES_USER" -d "$POSTGRES_DB" >/dev/null
}

register_user() {
  local username="$1"
  local password="$2"
  api POST /auth/register "" "$(json_body --arg u "$username" --arg p "$password" '{username:$u,password:$p}')" "200,409" "$SCRATCH_DIR/register-${username}.json" >/dev/null
}

login_user() {
  local username="$1"
  local password="$2"
  local out="$SCRATCH_DIR/login-${username}.json"
  api POST /auth/login "" "$(json_body --arg u "$username" --arg p "$password" '{username:$u,password:$p}')" 200 "$out" |
    jq -r '.data.token // .token // empty'
}

bootstrap_users() {
  register_user "$ADMIN_USERNAME" "$ADMIN_PASSWORD"
  register_user "$USER_A" "$USER_PASSWORD"
  register_user "$USER_B" "$USER_PASSWORD"
  ensure_admin_role "$ADMIN_USERNAME"

  ADMIN_TOKEN="$(login_user "$ADMIN_USERNAME" "$ADMIN_PASSWORD")"
  USER_TOKEN="$(login_user "$USER_A" "$USER_PASSWORD")"
  USER_B_TOKEN="$(login_user "$USER_B" "$USER_PASSWORD")"
  [[ -n "$ADMIN_TOKEN" && -n "$USER_TOKEN" && -n "$USER_B_TOKEN" ]] || die "login failed"

  {
    echo "# Linux Runtime Tokens"
    echo
    echo "- admin token acquired: yes"
    echo "- user token acquired: yes"
    echo "- user_b token acquired: yes"
  } >"$REPORT_DIR/tokens-summary.md"
}

create_problem() {
  local slug="linux-runtime-ab-$(date +%s)-$RANDOM"
  local body resp
  body="$(json_body --arg title "Linux Runtime A+B" --arg slug "$slug" '{
    title:$title,
    slug:$slug,
    statement:"Read two integers and print their sum.",
    visibility:"public",
    difficulty:"easy",
    tags:"linux,e2e",
    time_limit_ms:5000,
    memory_limit_mb:256
  }')"
  resp="$(api POST /problem/problems "$ADMIN_TOKEN" "$body" 200 "$SCRATCH_DIR/problem-create.json")"
  PROBLEM_ID="$(jq -r '.problem_id // .data.problem_id // empty' <<<"$resp")"
  [[ -n "$PROBLEM_ID" ]] || die "problem create failed: $resp"

  api POST "/problem/problems/${PROBLEM_ID}/test-cases" "$ADMIN_TOKEN" \
    '{"case_no":1,"input":"1 2\n","answer":"3\n","score":100,"sample":true,"hidden":false,"time_limit_ms":5000,"memory_limit_mb":256}' \
    200 "$SCRATCH_DIR/problem-case.json" >/dev/null
  api POST "/problem/problems/${PROBLEM_ID}/package/validate" "$ADMIN_TOKEN" '{}' 200 "$SCRATCH_DIR/problem-validate.json" >/dev/null
}

create_long_problem() {
  local slug="linux-runtime-long-$(date +%s)-$RANDOM"
  local body resp
  body="$(json_body --arg title "Linux Runtime Long Task" --arg slug "$slug" '{
    title:$title,
    slug:$slug,
    statement:"Used by worker lease/crash recovery checks.",
    visibility:"public",
    difficulty:"medium",
    tags:"linux,e2e,lease",
    time_limit_ms:20000,
    memory_limit_mb:256
  }')"
  resp="$(api POST /problem/problems "$ADMIN_TOKEN" "$body" 200 "$SCRATCH_DIR/long-problem-create.json")"
  LONG_PROBLEM_ID="$(jq -r '.problem_id // .data.problem_id // empty' <<<"$resp")"
  [[ -n "$LONG_PROBLEM_ID" ]] || die "long problem create failed: $resp"
  api POST "/problem/problems/${LONG_PROBLEM_ID}/test-cases" "$ADMIN_TOKEN" \
    '{"case_no":1,"input":"1 2\n","answer":"3\n","score":100,"sample":false,"hidden":false,"time_limit_ms":20000,"memory_limit_mb":256}' \
    200 "$SCRATCH_DIR/long-problem-case.json" >/dev/null
  api POST "/problem/problems/${LONG_PROBLEM_ID}/package/validate" "$ADMIN_TOKEN" '{}' 200 "$SCRATCH_DIR/long-problem-validate.json" >/dev/null
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
#include <cstdlib>
#include <vector>
#include <cstddef>
int main(){
  const std::size_t chunk = 16 * 1024 * 1024;
  std::vector<void*> chunks;
  while(true){
    volatile char *p = static_cast<volatile char*>(std::malloc(chunk));
    if(!p) return 0;
    for(std::size_t i = 0; i < chunk; i += 4096) p[i] = 1;
    chunks.push_back(const_cast<char*>(p));
  }
}
SRC
  cat >"$dir/cpp17/ole.cpp" <<'SRC'
#include <iostream>
int main(){ while(true) std::cout << "0123456789012345678901234567890123456789\n"; }
SRC
  cat >"$dir/cpp17/fork_bomb.cpp" <<'SRC'
#include <unistd.h>
int main(){ while(true){ fork(); } }
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
#include <stddef.h>
int main(){
  const size_t chunk = 16 * 1024 * 1024;
  void *chunks[65536];
  int n = 0;
  while(1){
    volatile char *p = (volatile char*)malloc(chunk);
    if(!p) return 0;
    for(size_t i = 0; i < chunk; i += 4096) p[i] = 1;
    chunks[n++ % 65536] = (void*)p;
  }
}
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
import ctypes

libc = ctypes.CDLL(None)
libc.malloc.argtypes = [ctypes.c_size_t]
libc.malloc.restype = ctypes.c_void_p
libc.memset.argtypes = [ctypes.c_void_p, ctypes.c_int, ctypes.c_size_t]

chunk = 64 * 1024 * 1024
chunks = []
while True:
    ptr = libc.malloc(chunk)
    if not ptr:
        raise MemoryError("malloc returned null")
    libc.memset(ptr, 1, chunk)
    chunks.append(ptr)
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
public class Main { public static void main(String[] args) { ArrayList<byte[]> x=new ArrayList<>(); while(true) x.add(new byte[16*1024*1024]); } }
SRC
  cat >"$dir/java17/MainOLE.java" <<'SRC'
public class Main { public static void main(String[] args) { while(true) System.out.println("0123456789012345678901234567890123456789"); } }
SRC
}

submit_code() {
  local token="$1"
  local problem_id="$2"
  local language="$3"
  local code_file="$4"
  local code resp id
  code="$(jq -Rs . <"$code_file")"
  resp="$(api POST /judge/submissions "$token" "{\"problem_id\":${problem_id},\"language\":\"${language}\",\"code\":${code}}" 200)"
  id="$(jq -r '.submission_id // .data.submission_id // empty' <<<"$resp")"
  [[ -n "$id" ]] || die "submit failed: $resp"
  printf '%s' "$id"
}

wait_submission() {
  local token="$1"
  local id="$2"
  local timeout_seconds="${3:-180}"
  local status=""
  for _ in $(seq 1 "$timeout_seconds"); do
    status="$(api GET "/judge/submissions/${id}" "$token" "" 200 | jq -r '.status // .data.status // empty')"
    if [[ -n "$status" && "$status" != "PENDING" && "$status" != "JUDGING" ]]; then
      printf '%s' "$status"
      return 0
    fi
    sleep 1
  done
  printf '%s' "$status"
  return 1
}

submission_detail() {
  local id="$1"
  api GET "/judge/submissions/${id}" "$ADMIN_TOKEN" "" 200
}

submission_cases() {
  local id="$1"
  api GET "/judge/submissions/${id}/cases" "$ADMIN_TOKEN" "" 200
}

record_matrix_row() {
  local language="$1"
  local case_type="$2"
  local submission_id="$3"
  local expected="$4"
  local actual="$5"
  local time_ms="$6"
  local memory_kb="$7"
  local worker_id="$8"
  local task_id="$9"
  local lease_version="${10}"
  printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
    "$language" "$case_type" "$submission_id" "$expected" "$actual" "$time_ms" "$memory_kb" "$worker_id" "$task_id" "$lease_version" >>"$MATRIX_TSV"
}

task_for_submission() {
  local submission_id="$1"
  api GET /judge/admin/tasks "$ADMIN_TOKEN" "" 200 |
    jq -r --argjson sid "$submission_id" '.tasks[]? | select(.submission_id == $sid) | [.task_id,.worker_id,.lease_version,.lease_expires_at,.status] | @tsv' |
    tail -n 1
}

submit_and_wait() {
  local token="$1"
  local problem_id="$2"
  local language="$3"
  local case_type="$4"
  local code_file="$5"
  local expected="$6"
  local id actual detail cases time_ms memory_kb task worker lease

  id="$(submit_code "$token" "$problem_id" "$language" "$code_file")"
  MATRIX_TOTAL=$((MATRIX_TOTAL + 1))
  actual="$(wait_submission "$token" "$id" 240 || true)"
  detail="$(submission_detail "$id")"
  cases="$(submission_cases "$id")"
  printf '%s' "$detail" >"$SCRATCH_DIR/submission-${id}.json"
  printf '%s' "$cases" >"$SCRATCH_DIR/submission-${id}-cases.json"

  time_ms="$(jq -r '.time_ms // .data.time_ms // 0' <<<"$detail")"
  memory_kb="$(jq -r '.memory_kb // .data.memory_kb // 0' <<<"$detail")"
  IFS=$'\t' read -r task worker lease _task_status <<<"$(task_for_submission "$id" || true)"
  task="${task:-}"
  worker="${worker:-}"
  lease="${lease:-}"

  record_matrix_row "$language" "$case_type" "$id" "$expected" "$actual" "$time_ms" "$memory_kb" "$worker" "$task" "$lease"
  echo "submission=$id language=$language case=$case_type expected=$expected actual=$actual memory_kb=$memory_kb worker=$worker"

  if [[ "$actual" != "$expected" ]]; then
    MATRIX_FAILED=$((MATRIX_FAILED + 1))
    return 1
  fi
  if [[ "${memory_kb:-0}" =~ ^[0-9]+$ && "$memory_kb" -gt 0 ]]; then
    MEMORY_NONZERO=$((MEMORY_NONZERO + 1))
  fi
}

run_status_matrix() {
  local srcdir="$SCRATCH_DIR/sources"
  rm -rf "$srcdir"
  write_sources "$srcdir"
  printf 'language\tcase_type\tsubmission_id\texpected_status\tactual_status\ttime_ms\tmemory_kb\tworker_id\ttask_id\tlease_version\n' >"$MATRIX_TSV"

  submit_and_wait "$USER_TOKEN" "$PROBLEM_ID" cpp17 ac "$srcdir/cpp17/ac.cpp" ACCEPTED
  submit_and_wait "$USER_TOKEN" "$PROBLEM_ID" cpp17 wa "$srcdir/cpp17/wa.cpp" WRONG_ANSWER
  submit_and_wait "$USER_TOKEN" "$PROBLEM_ID" cpp17 ce "$srcdir/cpp17/ce.cpp" COMPILE_ERROR
  submit_and_wait "$USER_TOKEN" "$PROBLEM_ID" cpp17 re "$srcdir/cpp17/re.cpp" RUNTIME_ERROR
  submit_and_wait "$USER_TOKEN" "$PROBLEM_ID" cpp17 tle "$srcdir/cpp17/tle.cpp" TIME_LIMIT_EXCEEDED
  submit_and_wait "$USER_TOKEN" "$PROBLEM_ID" cpp17 mle "$srcdir/cpp17/mle.cpp" MEMORY_LIMIT_EXCEEDED
  submit_and_wait "$USER_TOKEN" "$PROBLEM_ID" cpp17 ole "$srcdir/cpp17/ole.cpp" OUTPUT_LIMIT_EXCEEDED

  submit_and_wait "$USER_TOKEN" "$PROBLEM_ID" c11 ac "$srcdir/c11/ac.c" ACCEPTED
  submit_and_wait "$USER_TOKEN" "$PROBLEM_ID" c11 wa "$srcdir/c11/wa.c" WRONG_ANSWER
  submit_and_wait "$USER_TOKEN" "$PROBLEM_ID" c11 ce "$srcdir/c11/ce.c" COMPILE_ERROR
  submit_and_wait "$USER_TOKEN" "$PROBLEM_ID" c11 re "$srcdir/c11/re.c" RUNTIME_ERROR
  submit_and_wait "$USER_TOKEN" "$PROBLEM_ID" c11 tle "$srcdir/c11/tle.c" TIME_LIMIT_EXCEEDED
  submit_and_wait "$USER_TOKEN" "$PROBLEM_ID" c11 mle "$srcdir/c11/mle.c" MEMORY_LIMIT_EXCEEDED
  submit_and_wait "$USER_TOKEN" "$PROBLEM_ID" c11 ole "$srcdir/c11/ole.c" OUTPUT_LIMIT_EXCEEDED

  submit_and_wait "$USER_TOKEN" "$PROBLEM_ID" python3 ac "$srcdir/python3/ac.py" ACCEPTED
  submit_and_wait "$USER_TOKEN" "$PROBLEM_ID" python3 wa "$srcdir/python3/wa.py" WRONG_ANSWER
  submit_and_wait "$USER_TOKEN" "$PROBLEM_ID" python3 ce "$srcdir/python3/ce.py" COMPILE_ERROR
  submit_and_wait "$USER_TOKEN" "$PROBLEM_ID" python3 re "$srcdir/python3/re.py" RUNTIME_ERROR
  submit_and_wait "$USER_TOKEN" "$PROBLEM_ID" python3 tle "$srcdir/python3/tle.py" TIME_LIMIT_EXCEEDED
  submit_and_wait "$USER_TOKEN" "$PROBLEM_ID" python3 mle "$srcdir/python3/mle.py" MEMORY_LIMIT_EXCEEDED
  submit_and_wait "$USER_TOKEN" "$PROBLEM_ID" python3 ole "$srcdir/python3/ole.py" OUTPUT_LIMIT_EXCEEDED

  submit_and_wait "$USER_TOKEN" "$PROBLEM_ID" java17 ac "$srcdir/java17/MainAC.java" ACCEPTED
  submit_and_wait "$USER_TOKEN" "$PROBLEM_ID" java17 wa "$srcdir/java17/MainWA.java" WRONG_ANSWER
  submit_and_wait "$USER_TOKEN" "$PROBLEM_ID" java17 ce "$srcdir/java17/MainCE.java" COMPILE_ERROR
  submit_and_wait "$USER_TOKEN" "$PROBLEM_ID" java17 re "$srcdir/java17/MainRE.java" RUNTIME_ERROR
  submit_and_wait "$USER_TOKEN" "$PROBLEM_ID" java17 tle "$srcdir/java17/MainTLE.java" TIME_LIMIT_EXCEEDED
  submit_and_wait "$USER_TOKEN" "$PROBLEM_ID" java17 mle "$srcdir/java17/MainMLE.java" MEMORY_LIMIT_EXCEEDED
  submit_and_wait "$USER_TOKEN" "$PROBLEM_ID" java17 ole "$srcdir/java17/MainOLE.java" OUTPUT_LIMIT_EXCEEDED

  DISTINCT_MATRIX_WORKERS="$(tail -n +2 "$MATRIX_TSV" | awk -F '\t' '$8 != "" { seen[$8]=1 } END { print length(seen) }')"
  [[ "$MATRIX_FAILED" -eq 0 ]] || die "status matrix has $MATRIX_FAILED failures"
  [[ "$MEMORY_NONZERO" -gt 0 ]] || die "memory_kb remained zero for all terminal submissions"
}

wait_workers_online() {
  local expected="$1"
  local count=0
  for _ in $(seq 1 120); do
    count="$(api GET /judge/admin/workers "$ADMIN_TOKEN" "" 200 | tee "$SCRATCH_DIR/admin-workers.json" | jq '[.workers[]? | select(.status == "ONLINE" or .status == "BUSY")] | length')"
    if [[ "$count" -ge "$expected" ]]; then
      return 0
    fi
    sleep 1
  done
  die "expected $expected online workers, got $count"
}

run_double_worker_check() {
  echo "checking double worker claim distribution..."
  compose up -d --scale judge-worker=2 judge-worker >/dev/null
  wait_workers_online 2

  local src="$SCRATCH_DIR/sources/cpp17/tle.cpp"
  local id1 id2 running workers
  id1="$(submit_code "$USER_TOKEN" "$LONG_PROBLEM_ID" cpp17 "$src")"
  id2="$(submit_code "$USER_TOKEN" "$LONG_PROBLEM_ID" cpp17 "$src")"

  for _ in $(seq 1 40); do
    api GET /judge/admin/tasks "$ADMIN_TOKEN" "" 200 >"$SCRATCH_DIR/double-worker-tasks.json"
    running="$(jq --argjson a "$id1" --argjson b "$id2" '[.tasks[]? | select((.submission_id == $a or .submission_id == $b) and .status == "RUNNING")] | length' "$SCRATCH_DIR/double-worker-tasks.json")"
    workers="$(jq -r --argjson a "$id1" --argjson b "$id2" '[.tasks[]? | select(.submission_id == $a or .submission_id == $b) | .worker_id] | unique | length' "$SCRATCH_DIR/double-worker-tasks.json")"
    if [[ "$running" -ge 2 && "$workers" -ge 2 ]]; then
      DOUBLE_WORKER_OK=true
      break
    fi
    sleep 1
  done

  wait_submission "$USER_TOKEN" "$id1" 80 >/dev/null || true
  wait_submission "$USER_TOKEN" "$id2" 80 >/dev/null || true

  [[ "$DOUBLE_WORKER_OK" == true ]] || die "two long submissions were not observed running on two distinct workers"
}

container_for_worker_id() {
  local worker_id="$1"
  local cid
  for cid in $(compose ps -q judge-worker); do
    local hostname
    hostname="$(docker inspect -f '{{.Config.Hostname}}' "$cid")"
    if [[ "$hostname" == "$worker_id" ]]; then
      printf '%s' "$cid"
      return 0
    fi
  done
  return 1
}

run_crash_recovery_check() {
  echo "checking worker crash recovery..."
  compose up -d --scale judge-worker=2 judge-worker >/dev/null
  wait_workers_online 2

  local src="$SCRATCH_DIR/sources/cpp17/tle.cpp"
  local sid task_id worker_id lease_version lease_expires_at status cid new_worker new_lease stale_status final_status
  sid="$(submit_code "$USER_TOKEN" "$LONG_PROBLEM_ID" cpp17 "$src")"

  for _ in $(seq 1 40); do
    api GET /judge/admin/tasks "$ADMIN_TOKEN" "" 200 >"$SCRATCH_DIR/crash-tasks-before.json"
    read -r task_id worker_id lease_version lease_expires_at status < <(
      jq -r --argjson sid "$sid" '.tasks[]? | select(.submission_id == $sid and .status == "RUNNING") | [.task_id,.worker_id,.lease_version,.lease_expires_at,.status] | @tsv' "$SCRATCH_DIR/crash-tasks-before.json" | tail -n 1
    ) || true
    if [[ -n "${task_id:-}" && -n "${worker_id:-}" && "${lease_version:-}" =~ ^[0-9]+$ ]]; then
      break
    fi
    sleep 1
  done
  [[ -n "${task_id:-}" && -n "${worker_id:-}" ]] || die "crash recovery task was not claimed"

  cid="$(container_for_worker_id "$worker_id" || true)"
  [[ -n "$cid" ]] || die "could not map worker_id $worker_id to a judge-worker container"
  docker stop "$cid" >"$SCRATCH_DIR/crash-stopped-container.txt"

  for _ in $(seq 1 100); do
    api GET /judge/admin/tasks "$ADMIN_TOKEN" "" 200 >"$SCRATCH_DIR/crash-tasks-after.json"
    read -r new_worker new_lease status < <(
      jq -r --argjson sid "$sid" '.tasks[]? | select(.submission_id == $sid) | [.worker_id,.lease_version,.status] | @tsv' "$SCRATCH_DIR/crash-tasks-after.json" | tail -n 1
    ) || true
    if [[ "${status:-}" == "RUNNING" && -n "${new_worker:-}" && "$new_worker" != "$worker_id" && "${new_lease:-}" =~ ^[0-9]+$ && "$new_lease" -gt "$lease_version" ]]; then
      CRASH_RECOVERY_OK=true
      break
    fi
    sleep 1
  done

  local stale_body
  stale_body="$(json_body --arg wid "$worker_id" --arg status "ACCEPTED" --arg msg "stale lease should not win" --argjson lease "$lease_version" '{
    worker_id:$wid,
    lease_version:$lease,
    status:$status,
    score:100,
    time_ms:1,
    memory_kb:1,
    message:$msg,
    cases:[{case_no:1,status:"ACCEPTED",score:100,time_ms:1,memory_kb:1,message:"stale"}]
  }')"
  stale_status="$(worker_status POST "/judge/worker/tasks/${task_id}/result" "$stale_body")"
  if [[ "$stale_status" =~ ^(400|403|404|409|500)$ ]]; then
    STALE_LEASE_REJECTED=true
  fi

  final_status="$(wait_submission "$USER_TOKEN" "$sid" 120 || true)"
  if [[ "$final_status" == "ACCEPTED" ]]; then
    die "stale result overwrote crash recovery submission"
  fi
  if [[ -z "$final_status" || "$final_status" == "PENDING" || "$final_status" == "JUDGING" ]]; then
    die "crash recovery submission did not reach a terminal status: $final_status"
  fi

  {
    echo "# Worker Crash Recovery"
    echo
    echo "- submission_id: $sid"
    echo "- task_id: $task_id"
    echo "- old_worker_id: $worker_id"
    echo "- old_lease_version: $lease_version"
    echo "- old_lease_expires_at: $lease_expires_at"
    echo "- new_worker_id: ${new_worker:-}"
    echo "- new_lease_version: ${new_lease:-}"
    echo "- recovery_observed: $CRASH_RECOVERY_OK"
    echo "- stale_result_http_status: $stale_status"
    echo "- stale_lease_rejected: $STALE_LEASE_REJECTED"
    echo "- final_submission_status: $final_status"
  } >"$CRASH_REPORT"

  compose up -d --scale judge-worker=2 judge-worker >/dev/null

  [[ "$CRASH_RECOVERY_OK" == true ]] || die "lease was not reclaimed by another worker"
  [[ "$STALE_LEASE_REJECTED" == true ]] || die "stale lease result was not rejected"
}

run_fork_bomb_check() {
  echo "checking fork bomb containment..."
  local id status
  id="$(submit_code "$USER_TOKEN" "$PROBLEM_ID" cpp17 "$SCRATCH_DIR/sources/cpp17/fork_bomb.cpp")"
  status="$(wait_submission "$USER_TOKEN" "$id" 120 || true)"
  echo "fork_bomb submission=$id status=$status" >"$REPORT_DIR/fork-bomb.md"
  case "$status" in
    RUNTIME_ERROR|TIME_LIMIT_EXCEEDED|MEMORY_LIMIT_EXCEEDED) ;;
    *) die "fork bomb returned unexpected status: $status" ;;
  esac
}

check_no_leftover_processes() {
  echo "checking leftover nsjail/user processes..."
  local found=0
  : >"$REPORT_DIR/process-cleanup.md"
  for cid in $(compose ps -q judge-worker); do
    {
      echo "## container $cid"
      docker exec "$cid" sh -lc "ps -ef | grep -E 'nsjail|/work/main|MainTLE|main.py' | grep -v grep || true"
      echo
    } >>"$REPORT_DIR/process-cleanup.md"
  done
  if grep -Eq 'nsjail|/work/main|MainTLE|main.py' "$REPORT_DIR/process-cleanup.md"; then
    found=1
  fi
  [[ "$found" -eq 0 ]] || die "leftover nsjail/user processes found"
}

run_redis_signal_check() {
  echo "checking Redis signal history..."
  local xlen xpending queue trim
  xlen="$(compose exec -T redis redis-cli XLEN ojos:judge:submissions | tr -d '\r')"
  xpending="$(compose exec -T redis redis-cli XPENDING ojos:judge:submissions judge-workers 2>&1 | tr -d '\r' || true)"
  queue="$(api GET /judge/admin/queue "$ADMIN_TOKEN" "" 200)"
  printf '%s' "$queue" >"$SCRATCH_DIR/admin-queue.json"
  trim="$(jq -r '.trim_strategy // empty' <<<"$queue")"
  {
    echo "# Redis Signal History"
    echo
    echo "- stream: ojos:judge:submissions"
    echo "- xlen: $xlen"
    echo "- xpending: $xpending"
    echo "- queue_api: $(jq -c '.' <<<"$queue")"
  } >"$REPORT_DIR/redis-signal-history.md"

  if [[ "$xlen" =~ ^[0-9]+$ && "$xlen" -gt 0 && "$trim" == *"MAXLEN"* && "$trim" == *"PostgreSQL judge_tasks"* ]]; then
    REDIS_SIGNAL_OK=true
  fi
  [[ "$REDIS_SIGNAL_OK" == true ]] || die "Redis signal history check failed"
}

run_admin_health_check() {
  local health
  health="$(api GET /admin/health "$ADMIN_TOKEN" "" 200)"
  printf '%s' "$health" >"$REPORT_DIR/admin-health.json"
  ADMIN_HEALTH_STATUS="$(jq -r '.status // empty' <<<"$health")"
  ADMIN_HEALTH_JUDGE_STATUS="$(jq -r '.components[]? | select(.name == "judge") | .status' <<<"$health" | head -n 1)"
  [[ "$ADMIN_HEALTH_STATUS" == "ok" ]] || die "admin health expected ok, got $ADMIN_HEALTH_STATUS"
  [[ "$ADMIN_HEALTH_JUDGE_STATUS" == "ok" ]] || die "admin health judge expected ok, got $ADMIN_HEALTH_JUDGE_STATUS"
}

run_permission_rescan() {
  echo "checking permission denials..."
  local checks=(
    "GET /admin/health"
    "GET /admin/modules"
    "GET /judge/admin/queue"
    "GET /judge/admin/workers"
    "GET /judge/admin/tasks"
  )
  : >"$REPORT_DIR/permission-rescan.md"
  for check in "${checks[@]}"; do
    local method="${check%% *}"
    local path="${check#* }"
    local user_status none_status
    user_status="$(api_status "$method" "$path" "$USER_TOKEN")"
    none_status="$(api_status "$method" "$path")"
    echo "- $method $path user=$user_status none=$none_status" >>"$REPORT_DIR/permission-rescan.md"
    if [[ "$user_status" != "403" || "$none_status" != "401" ]]; then
      PERMISSION_FAILURES=$((PERMISSION_FAILURES + 1))
    fi
  done

  local own other status
  own="$(submit_code "$USER_TOKEN" "$PROBLEM_ID" python3 "$SCRATCH_DIR/sources/python3/ac.py")"
  wait_submission "$USER_TOKEN" "$own" 120 >/dev/null || true
  other="$(submit_code "$USER_B_TOKEN" "$PROBLEM_ID" python3 "$SCRATCH_DIR/sources/python3/ac.py")"
  wait_submission "$USER_B_TOKEN" "$other" 120 >/dev/null || true
  status="$(api_status GET "/judge/submissions/${other}/debug-logs" "$USER_TOKEN")"
  echo "- GET /judge/submissions/$other/debug-logs user_other=$status" >>"$REPORT_DIR/permission-rescan.md"
  if [[ "$status" != "403" && "$status" != "404" ]]; then
    PERMISSION_FAILURES=$((PERMISSION_FAILURES + 1))
  fi

  [[ "$PERMISSION_FAILURES" -eq 0 ]] || die "permission rescan found $PERMISSION_FAILURES failures"
}

run_path_leak_scan() {
  echo "checking path leaks..."
  local scan_file="$REPORT_DIR/path-leak-scan.txt"
  : >"$scan_file"
  local forbidden='code_path|result_path|package_dir|stdout_path|stderr_path|checker_log_path|(^|[^A-Za-z0-9_-])/work(/|$)|/sys/fs/cgroup|storage/problems|storage/submissions|/data/ojos|/var/lib/ojos|/mnt/d|D:\\'
  if grep -RInE "$forbidden" "$SCRATCH_DIR" "$REPORT_DIR" \
    --binary-files=without-match \
    --exclude-dir='cargo-home' \
    --exclude-dir='cargo-target' \
    --exclude='path-leak-scan.txt' \
    --exclude='worker-crash-recovery.md' \
    --exclude='process-cleanup.md' \
    --exclude='redis-signal-history.md' \
    --exclude='worker-container-capabilities.md' \
    >"$scan_file"; then
    PATH_LEAKS="$(wc -l <"$scan_file" | tr -d ' ')"
  else
    PATH_LEAKS=0
  fi
  [[ "$PATH_LEAKS" -eq 0 ]] || die "path leak scan found $PATH_LEAKS findings"
}

check_worker_container_capabilities() {
  echo "checking worker container nsjail/cgroup..."
  local cid
  cid="$(compose ps -q judge-worker | head -n 1)"
  [[ -n "$cid" ]] || die "judge-worker container not found"
  {
    echo "# Worker Container Capability Check"
    echo
    docker exec "$cid" sh -lc 'which nsjail; nsjail --version || true; test -f /sys/fs/cgroup/cgroup.controllers && cat /sys/fs/cgroup/cgroup.controllers; test -w /sys/fs/cgroup && echo cgroup_root_writable=yes || echo cgroup_root_writable=no'
  } >"$REPORT_DIR/worker-container-capabilities.md"
  grep -q 'nsjail' "$REPORT_DIR/worker-container-capabilities.md" || die "nsjail not found in worker container"
  grep -q 'memory' "$REPORT_DIR/worker-container-capabilities.md" || die "worker container cgroup memory controller not visible"
  grep -q 'pids' "$REPORT_DIR/worker-container-capabilities.md" || die "worker container cgroup pids controller not visible"
}

write_summary() {
  jq -n \
    --arg admin_health_status "$ADMIN_HEALTH_STATUS" \
    --arg admin_health_judge_status "$ADMIN_HEALTH_JUDGE_STATUS" \
    --argjson matrix_total "$MATRIX_TOTAL" \
    --argjson matrix_failed "$MATRIX_FAILED" \
    --argjson memory_nonzero "$MEMORY_NONZERO" \
    --argjson path_leaks "$PATH_LEAKS" \
    --argjson permission_failures "$PERMISSION_FAILURES" \
    --argjson distinct_matrix_workers "$DISTINCT_MATRIX_WORKERS" \
    --arg double_worker_ok "$DOUBLE_WORKER_OK" \
    --arg crash_recovery_ok "$CRASH_RECOVERY_OK" \
    --arg stale_lease_rejected "$STALE_LEASE_REJECTED" \
    --arg redis_signal_ok "$REDIS_SIGNAL_OK" \
    '{
      admin_health_status:$admin_health_status,
      admin_health_judge_status:$admin_health_judge_status,
      matrix_total:$matrix_total,
      matrix_failed:$matrix_failed,
      memory_nonzero_count:$memory_nonzero,
      distinct_matrix_workers:$distinct_matrix_workers,
      double_worker_ok:($double_worker_ok == "true"),
      crash_recovery_ok:($crash_recovery_ok == "true"),
      stale_lease_rejected:($stale_lease_rejected == "true"),
      redis_signal_ok:($redis_signal_ok == "true"),
      path_leaks:$path_leaks,
      permission_failures:$permission_failures
    }' | tee "$SUMMARY_JSON"
}

main() {
  require_cmd curl
  require_cmd jq
  require_cmd docker

  test -f /sys/fs/cgroup/cgroup.controllers || die "cgroup v2 is required: /sys/fs/cgroup/cgroup.controllers missing"
  grep -qw memory /sys/fs/cgroup/cgroup.controllers || die "cgroup v2 memory controller missing"
  grep -qw pids /sys/fs/cgroup/cgroup.controllers || die "cgroup v2 pids controller missing"

  if [[ "${OJOS_SKIP_DOCKER:-0}" != "1" ]]; then
    compose up -d --build
  fi
  compose ps | tee "$LOG_DIR/compose-ps.txt"
  compose logs --tail=300 >"$LOG_DIR/compose-logs.txt"

  apply_migrations
  compose up -d >/dev/null
  wait_for_gateway
  bootstrap_users
  create_problem
  create_long_problem

  compose up -d --scale judge-worker=2 judge-worker >/dev/null
  wait_workers_online 2
  check_worker_container_capabilities
  run_status_matrix
  run_fork_bomb_check
  check_no_leftover_processes
  run_double_worker_check
  run_crash_recovery_check
  run_redis_signal_check
  run_admin_health_check
  run_permission_rescan
  run_path_leak_scan
  write_summary

  echo "E2E Linux Judge Runtime acceptance completed."
}

main "$@"
