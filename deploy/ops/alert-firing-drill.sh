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
blackbox="ojos-alert-blackbox-$run_id"
prometheus_image="${OJOS_DRILL_PROMETHEUS_IMAGE:-prom/prometheus:v2.55.1}"
alertmanager_image="${OJOS_DRILL_ALERTMANAGER_IMAGE:-prom/alertmanager:v0.27.0}"
blackbox_image="${OJOS_DRILL_BLACKBOX_IMAGE:-prom/blackbox-exporter:v0.25.0}"
webhook_port="${OJOS_ALERT_DRILL_WEBHOOK_PORT:-19094}"
health_port="${OJOS_ALERT_DRILL_HEALTH_PORT:-19095}"
target_alerts_json='["OJOSServiceDown","OJOSJudgeWorkerOffline","OJOSJudgeQueueBacklog","OJOSHighHTTP5xxRate","OJOSBackupStale"]'
status="failed"
start_ts="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
webhook_pid=""
health_pid=""

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
  docker logs "$blackbox" >"$evidence_dir/logs/blackbox.log" 2>&1 || true
  docker rm -f "$prometheus" "$alertmanager" "$blackbox" >/dev/null 2>&1 || true
  docker network rm "$network" >/dev/null 2>&1 || true
  if [[ -n "$webhook_pid" ]]; then
    kill "$webhook_pid" >/dev/null 2>&1 || true
    wait "$webhook_pid" >/dev/null 2>&1 || true
  fi
  if [[ -n "$health_pid" ]]; then
    kill "$health_pid" >/dev/null 2>&1 || true
    wait "$health_pid" >/dev/null 2>&1 || true
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
      rule_names: ["OJOSServiceDown", "OJOSJudgeWorkerOffline", "OJOSJudgeQueueBacklog", "OJOSHighHTTP5xxRate", "OJOSBackupStale"],
      evidence: {
        log: "logs/alert-firing-drill.log",
        prometheus_rules: "prometheus-rules.json",
        alertmanager_alerts: "alertmanager-alerts.json",
        firing_webhook_bodies: "webhook/firing-*.json",
        resolved_webhook_bodies: "webhook/resolved-*.json"
      }
    }' >"$evidence_dir/manifest.json" || true
  if [[ $rc -eq 0 ]]; then
    echo "[OK] service-down, worker-offline, queue-backlog, 5xx and backup-stale fired from real scraped facts and all resolved after recovery"
  else
    echo "[FAILED] alert firing drill failed; evidence=$evidence_dir" >&2
  fi
  exit "$rc"
}
trap finish EXIT

need_cmd docker
need_cmd jq
need_cmd python3
need_cmd curl

# Validate the production alert expressions before exercising the delivery
# pipeline. This prevents the drill-only rule from masking broken real rules.
docker_run --rm \
  -v "$(host_mount_path "$repo_root/deploy/ops/monitoring"):/rules:ro" \
  --entrypoint /bin/promtool \
  "$prometheus_image" \
  check rules /rules/alerts.yml
docker_run --rm \
  -w /rules \
  -v "$(host_mount_path "$repo_root/deploy/ops/monitoring"):/rules:ro" \
  --entrypoint /bin/promtool \
  "$prometheus_image" \
  test rules alert-tests.yml

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
  - job_name: ojos-health
    metrics_path: /probe
    params:
      module:
        - http_2xx
    static_configs:
      - targets:
          - http://host.docker.internal:__OJOS_ALERT_DRILL_HEALTH_PORT__/health
        labels:
          service: drill-service
    relabel_configs:
      - source_labels:
          - __address__
        target_label: __param_target
      - source_labels:
          - __param_target
        target_label: instance
      - target_label: __address__
        replacement: blackbox-exporter:9115
  - job_name: ojos-drill-runtime
    metrics_path: /metrics
    static_configs:
      - targets:
          - host.docker.internal:__OJOS_ALERT_DRILL_HEALTH_PORT__
YAML
sed -i "s/__OJOS_ALERT_DRILL_HEALTH_PORT__/$health_port/g" "$evidence_dir/config/prometheus.yml"

cat >"$evidence_dir/config/blackbox.yml" <<'YAML'
modules:
  http_2xx:
    prober: http
    timeout: 2s
    http:
      method: GET
      preferred_ip_protocol: ip4
YAML

cat >"$evidence_dir/config/alertmanager.yml" <<YAML
route:
  receiver: drill-webhook
  group_by:
    - alertname
  group_wait: 0s
  group_interval: 1s
  repeat_interval: 1h
receivers:
  - name: drill-webhook
    webhook_configs:
      - url: "http://host.docker.internal:$webhook_port/alert"
        send_resolved: true
YAML

python3 - "$evidence_dir/webhook" "$webhook_port" >"$evidence_dir/logs/webhook.log" 2>&1 <<'PY' &
import http.server
import pathlib
import json
import sys

target_dir = pathlib.Path(sys.argv[1])
port = int(sys.argv[2])

class Handler(http.server.BaseHTTPRequestHandler):
    def do_POST(self):
        size = int(self.headers.get("content-length", "0"))
        body = self.rfile.read(size)
        payload = json.loads(body)
        for alert in payload.get("alerts", []):
            alertname = alert.get("labels", {}).get("alertname", "")
            if alertname in {
                "OJOSServiceDown",
                "OJOSJudgeWorkerOffline",
                "OJOSJudgeQueueBacklog",
                "OJOSHighHTTP5xxRate",
                "OJOSBackupStale",
            }:
                status = alert.get("status", "unknown")
                (target_dir / f"{status}-{alertname}.json").write_bytes(body)
        self.send_response(200)
        self.end_headers()
        self.wfile.write(b"ok")
    def log_message(self, fmt, *args):
        print(fmt % args)

# Alertmanager runs in a Docker network and reaches the host through
# host.docker.internal. Binding only to loopback makes that connection fail on
# Linux runners even though the host-side curl checks still work.
http.server.ThreadingHTTPServer(("0.0.0.0", port), Handler).serve_forever()
PY
webhook_pid="$!"

write_drill_state() {
  local health_status="$1"
  local workers_online="$2"
  local queue_pending="$3"
  local backup_timestamp="$4"
  local emit_5xx="$5"
  local temporary="$evidence_dir/config/drill-state.json.tmp"
  jq -n \
    --argjson health_status "$health_status" \
    --argjson workers_online "$workers_online" \
    --argjson queue_pending "$queue_pending" \
    --argjson backup_timestamp "$backup_timestamp" \
    --argjson emit_5xx "$emit_5xx" \
    '{health_status:$health_status,workers_online:$workers_online,queue_pending:$queue_pending,backup_timestamp:$backup_timestamp,emit_5xx:$emit_5xx}' \
    >"$temporary"
  mv "$temporary" "$evidence_dir/config/drill-state.json"
}

# These are real, scrapeable exporter facts. The fault phase deliberately
# mirrors authoritative Judge PostgreSQL gauges, the shared HTTP middleware
# counter, the node-exporter backup textfile gauge and a blackbox health probe.
write_drill_state 503 0 101 0 true
python3 - "$evidence_dir/config/drill-state.json" "$health_port" >"$evidence_dir/logs/health-endpoint.log" 2>&1 <<'PY' &
import http.server
import json
import pathlib
import sys

status_file = pathlib.Path(sys.argv[1])
port = int(sys.argv[2])
request_5xx_total = 0
last_state = {
    "health_status": 503,
    "workers_online": 0,
    "queue_pending": 101,
    "backup_timestamp": 0,
    "emit_5xx": True,
}

def state():
    global last_state
    try:
        candidate = json.loads(status_file.read_text(encoding="utf-8"))
        required = {
            "health_status",
            "workers_online",
            "queue_pending",
            "backup_timestamp",
            "emit_5xx",
        }
        if set(candidate) == required:
            last_state = candidate
    except (OSError, json.JSONDecodeError):
        pass
    return last_state

class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        global request_5xx_total
        current = state()
        if self.path == "/health":
            status = int(current["health_status"])
            self.send_response(status)
            self.end_headers()
            self.wfile.write(b"healthy" if status == 200 else b"unavailable")
            return
        if self.path == "/metrics":
            if current["emit_5xx"]:
                request_5xx_total += 1
            body = (
                "# TYPE ojos_judge_workers_online gauge\n"
                f"ojos_judge_workers_online{{instance=\"drill\"}} {int(current['workers_online'])}\n"
                "# TYPE ojos_judge_queue_pending_tasks gauge\n"
                f"ojos_judge_queue_pending_tasks{{instance=\"drill\"}} {int(current['queue_pending'])}\n"
                "# TYPE ojos_http_requests_total counter\n"
                f"ojos_http_requests_total{{service=\"drill-service\",method=\"GET\",status=\"503\"}} {request_5xx_total}\n"
                "# TYPE ojos_backup_last_success_timestamp_seconds gauge\n"
                f"ojos_backup_last_success_timestamp_seconds{{environment=\"drill\"}} {int(current['backup_timestamp'])}\n"
            ).encode("ascii")
            self.send_response(200)
            self.send_header("Content-Type", "text/plain; version=0.0.4")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
            return
        self.send_response(404)
        self.end_headers()
    def log_message(self, fmt, *args):
        print(fmt % args)

http.server.ThreadingHTTPServer(("0.0.0.0", port), Handler).serve_forever()
PY
health_pid="$!"

docker network create "$network" >/dev/null
docker_run -d \
  --name "$blackbox" \
  --network "$network" \
  --network-alias blackbox-exporter \
  --add-host=host.docker.internal:host-gateway \
  -v "$(host_mount_path "$evidence_dir/config/blackbox.yml"):/etc/blackbox_exporter/config.yml:ro" \
  "$blackbox_image" \
  --config.file=/etc/blackbox_exporter/config.yml >/dev/null
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
  --add-host=host.docker.internal:host-gateway \
  -p 127.0.0.1::9090 \
  -v "$(host_mount_path "$evidence_dir/config/prometheus.yml"):/etc/prometheus/prometheus.yml:ro" \
  -v "$(host_mount_path "$repo_root/deploy/ops/monitoring/alerts.yml"):/etc/prometheus/alerts.yml:ro" \
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

for _ in $(seq 1 "${OJOS_ALERT_DRILL_FIRE_TIMEOUT_SECONDS:-780}"); do
  curl -fsS "http://127.0.0.1:$prom_port/api/v1/rules" >"$evidence_dir/prometheus-rules.json"
  if jq -e --argjson wanted "$target_alerts_json" \
    '[.data.groups[].rules[] | select(.name as $name | $wanted | index($name)) | select(.state == "firing") | .name] | unique | length == ($wanted | length)' \
    "$evidence_dir/prometheus-rules.json" >/dev/null; then
    break
  fi
  sleep 1
done
jq -e --argjson wanted "$target_alerts_json" \
  '[.data.groups[].rules[] | select(.name as $name | $wanted | index($name)) | select(.state == "firing") | .name] | unique | length == ($wanted | length)' \
  "$evidence_dir/prometheus-rules.json" >/dev/null

am_port="$(docker inspect -f '{{(index (index .NetworkSettings.Ports "9093/tcp") 0).HostPort}}' "$alertmanager")"
for _ in $(seq 1 60); do
  curl -fsS "http://127.0.0.1:$am_port/api/v2/alerts" >"$evidence_dir/alertmanager-alerts.json" || true
  if jq -e --argjson wanted "$target_alerts_json" \
    '[.[] | select(.labels.alertname as $name | $wanted | index($name)) | .labels.alertname] | unique | length == ($wanted | length)' \
    "$evidence_dir/alertmanager-alerts.json" >/dev/null; then
    break
  fi
  sleep 1
done
jq -e --argjson wanted "$target_alerts_json" \
  '[.[] | select(.labels.alertname as $name | $wanted | index($name)) | .labels.alertname] | unique | length == ($wanted | length)' \
  "$evidence_dir/alertmanager-alerts.json" >/dev/null

for alertname in OJOSServiceDown OJOSJudgeWorkerOffline OJOSJudgeQueueBacklog OJOSHighHTTP5xxRate OJOSBackupStale; do
  for _ in $(seq 1 60); do
    if [[ -s "$evidence_dir/webhook/firing-$alertname.json" ]]; then
      break
    fi
    sleep 1
  done
  test -s "$evidence_dir/webhook/firing-$alertname.json"
  jq -e --arg alertname "$alertname" \
    '.alerts[] | select(.labels.alertname == $alertname and .status == "firing")' \
    "$evidence_dir/webhook/firing-$alertname.json" >/dev/null
done

write_drill_state 200 1 0 "$(date -u +%s)" false
for _ in $(seq 1 "${OJOS_ALERT_DRILL_RESOLVE_TIMEOUT_SECONDS:-420}"); do
  curl -fsS "http://127.0.0.1:$prom_port/api/v1/rules" >"$evidence_dir/prometheus-rules.json"
  if jq -e --argjson wanted "$target_alerts_json" \
    '[.data.groups[].rules[] | select(.name as $name | $wanted | index($name)) | select(.state == "firing")] | length == 0' \
    "$evidence_dir/prometheus-rules.json" >/dev/null; then
    break
  fi
  sleep 1
done
jq -e --argjson wanted "$target_alerts_json" \
  '[.data.groups[].rules[] | select(.name as $name | $wanted | index($name)) | select(.state == "firing")] | length == 0' \
  "$evidence_dir/prometheus-rules.json" >/dev/null

for alertname in OJOSServiceDown OJOSJudgeWorkerOffline OJOSJudgeQueueBacklog OJOSHighHTTP5xxRate OJOSBackupStale; do
  for _ in $(seq 1 60); do
    if [[ -s "$evidence_dir/webhook/resolved-$alertname.json" ]]; then
      break
    fi
    sleep 1
  done
  test -s "$evidence_dir/webhook/resolved-$alertname.json"
  jq -e --arg alertname "$alertname" \
    '.alerts[] | select(.labels.alertname == $alertname and .status == "resolved")' \
    "$evidence_dir/webhook/resolved-$alertname.json" >/dev/null
done
