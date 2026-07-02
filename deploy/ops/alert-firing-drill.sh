#!/usr/bin/env bash
set -Eeuo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$script_dir/../.." && pwd)"
run_id="${OJOS_ALERT_DRILL_RUN_ID:-$(date -u +%Y%m%dT%H%M%SZ)-$$}"
evidence_dir="${OJOS_EVIDENCE_DIR:-$repo_root/artifacts/alert-firing-drill/$run_id}"
mkdir -p "$evidence_dir"
evidence_dir="$(cd "$evidence_dir" && pwd)"
mkdir -p "$evidence_dir/logs" "$evidence_dir/config" "$evidence_dir/webhook"
log_file="$evidence_dir/logs/alert-firing-drill.log"
exec > >(tee -a "$log_file") 2>&1
export NO_PROXY="${NO_PROXY:-localhost,127.0.0.1,::1},localhost,127.0.0.1,::1"
export no_proxy="${no_proxy:-$NO_PROXY}"

network="ojos-alert-drill-$run_id"
prometheus="ojos-alert-prometheus-$run_id"
alertmanager="ojos-alertmanager-$run_id"
prometheus_image="${OJOS_DRILL_PROMETHEUS_IMAGE:-prom/prometheus:v2.55.1}"
alertmanager_image="${OJOS_DRILL_ALERTMANAGER_IMAGE:-prom/alertmanager:v0.27.0}"
webhook_port="${OJOS_ALERT_DRILL_WEBHOOK_PORT:-19094}"
status="failed"
start_ts="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
webhook_pid=""

docker_run() {
  MSYS2_ARG_CONV_EXCL='*' docker run "$@"
}

need_cmd() {
  command -v "$1" >/dev/null 2>&1 || {
    cat >&2 <<EOF
[ENV-BLOCKED] alert-firing-drill
命令：command -v $1
错误摘要：$1 not found
判断：环境问题
是否阻塞当前任务：是
最小修复建议：安装或启动缺失依赖后重跑 alert firing drill
后续处理：需要用户介入
EOF
    exit 127
  }
}

host_mount_path() {
  if command -v cygpath >/dev/null 2>&1; then
    cygpath -w "$1"
  else
    printf '%s' "$1"
  fi
}

finish() {
  local rc=$?
  [[ $rc -eq 0 ]] && status="passed" || status="failed"
  docker logs "$prometheus" >"$evidence_dir/logs/prometheus.log" 2>&1 || true
  docker logs "$alertmanager" >"$evidence_dir/logs/alertmanager.log" 2>&1 || true
  docker rm -f "$prometheus" "$alertmanager" >/dev/null 2>&1 || true
  docker network rm "$network" >/dev/null 2>&1 || true
  if [[ -n "$webhook_pid" ]]; then
    kill "$webhook_pid" >/dev/null 2>&1 || true
    wait "$webhook_pid" >/dev/null 2>&1 || true
  fi
  jq -n \
    --arg status "$status" \
    --arg start_ts "$start_ts" \
    --arg end_ts "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
    '{
      drill: "observability-alert-firing",
      status: $status,
      start_timestamp: $start_ts,
      end_timestamp: $end_ts,
      rule_name: "OJOSDrillAlwaysFiring",
      evidence: {
        log: "logs/alert-firing-drill.log",
        prometheus_rules: "prometheus-rules.json",
        alertmanager_alerts: "alertmanager-alerts.json",
        webhook_body: "webhook/alert.json"
      }
    }' >"$evidence_dir/manifest.json" || true
  if [[ $rc -eq 0 ]]; then
    echo "[OK] at least one alert fires and reaches Alertmanager in drill"
  else
    echo "[FAILED] alert firing drill failed; evidence=$evidence_dir" >&2
  fi
  exit "$rc"
}
trap finish EXIT

need_cmd docker
need_cmd jq
need_cmd python3

cat >"$evidence_dir/config/alerts.yml" <<'YAML'
groups:
  - name: ojos-drill
    rules:
      - alert: OJOSDrillAlwaysFiring
        expr: vector(1)
        for: 0s
        labels:
          severity: critical
          service: drill
        annotations:
          summary: "OJOS drill alert"
YAML

cat >"$evidence_dir/config/prometheus.yml" <<'YAML'
global:
  scrape_interval: 1s
  evaluation_interval: 1s
rule_files:
  - /etc/prometheus/alerts.yml
alerting:
  alertmanagers:
    - static_configs:
        - targets:
            - alertmanager:9093
scrape_configs:
  - job_name: prometheus
    static_configs:
      - targets:
          - localhost:9090
YAML

cat >"$evidence_dir/config/alertmanager.yml" <<YAML
route:
  receiver: drill-webhook
  group_wait: 0s
  group_interval: 1s
  repeat_interval: 1h
receivers:
  - name: drill-webhook
    webhook_configs:
      - url: "http://host.docker.internal:$webhook_port/alert"
        send_resolved: false
YAML

python3 - "$evidence_dir/webhook/alert.json" "$webhook_port" >"$evidence_dir/logs/webhook.log" 2>&1 <<'PY' &
import http.server
import pathlib
import sys

target = pathlib.Path(sys.argv[1])
port = int(sys.argv[2])

class Handler(http.server.BaseHTTPRequestHandler):
    def do_POST(self):
        size = int(self.headers.get("content-length", "0"))
        body = self.rfile.read(size)
        target.write_bytes(body)
        self.send_response(200)
        self.end_headers()
        self.wfile.write(b"ok")
    def log_message(self, fmt, *args):
        print(fmt % args)

http.server.ThreadingHTTPServer(("127.0.0.1", port), Handler).serve_forever()
PY
webhook_pid="$!"

docker network create "$network" >/dev/null
docker_run -d \
  --name "$alertmanager" \
  --network "$network" \
  --network-alias alertmanager \
  --add-host=host.docker.internal:host-gateway \
  -p 127.0.0.1::9093 \
  -v "$(host_mount_path "$evidence_dir/config/alertmanager.yml"):/etc/alertmanager/alertmanager.yml:ro" \
  "$alertmanager_image" \
  --config.file=/etc/alertmanager/alertmanager.yml \
  --storage.path=/alertmanager >/dev/null

docker_run -d \
  --name "$prometheus" \
  --network "$network" \
  -p 127.0.0.1::9090 \
  -v "$(host_mount_path "$evidence_dir/config/prometheus.yml"):/etc/prometheus/prometheus.yml:ro" \
  -v "$(host_mount_path "$evidence_dir/config/alerts.yml"):/etc/prometheus/alerts.yml:ro" \
  "$prometheus_image" \
  --config.file=/etc/prometheus/prometheus.yml \
  --storage.tsdb.path=/prometheus >/dev/null

prom_port="$(docker inspect -f '{{(index (index .NetworkSettings.Ports "9090/tcp") 0).HostPort}}' "$prometheus")"
for _ in $(seq 1 60); do
  if curl -fsS "http://127.0.0.1:$prom_port/-/ready" >/dev/null 2>&1; then
    break
  fi
  sleep 1
done
curl -fsS "http://127.0.0.1:$prom_port/-/ready" >/dev/null

for _ in $(seq 1 60); do
  curl -fsS "http://127.0.0.1:$prom_port/api/v1/rules" >"$evidence_dir/prometheus-rules.json"
  if jq -e '.data.groups[].rules[] | select(.name == "OJOSDrillAlwaysFiring" and .state == "firing")' "$evidence_dir/prometheus-rules.json" >/dev/null; then
    break
  fi
  sleep 1
done
jq -e '.data.groups[].rules[] | select(.name == "OJOSDrillAlwaysFiring" and .state == "firing")' "$evidence_dir/prometheus-rules.json" >/dev/null

am_port="$(docker inspect -f '{{(index (index .NetworkSettings.Ports "9093/tcp") 0).HostPort}}' "$alertmanager")"
for _ in $(seq 1 60); do
  curl -fsS "http://127.0.0.1:$am_port/api/v2/alerts" >"$evidence_dir/alertmanager-alerts.json" || true
  if jq -e '.[] | select(.labels.alertname == "OJOSDrillAlwaysFiring")' "$evidence_dir/alertmanager-alerts.json" >/dev/null; then
    break
  fi
  sleep 1
done
jq -e '.[] | select(.labels.alertname == "OJOSDrillAlwaysFiring")' "$evidence_dir/alertmanager-alerts.json" >/dev/null

for _ in $(seq 1 60); do
  if [[ -s "$evidence_dir/webhook/alert.json" ]]; then
    break
  fi
  sleep 1
done
test -s "$evidence_dir/webhook/alert.json"
jq -e '.alerts[] | select(.labels.alertname == "OJOSDrillAlwaysFiring")' "$evidence_dir/webhook/alert.json" >/dev/null
