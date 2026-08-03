#!/bin/sh
set -eu

MAX_RESPONSE_BYTES=4096
MIN_PROBE_PORT=${OJOS_CAPACITY_PROBE_MIN_PORT:-20000}
MAX_PROBE_PORT=${OJOS_CAPACITY_PROBE_MAX_PORT:-20199}
PROBE_TIMEOUT_SECONDS=${OJOS_CAPACITY_PROBE_TIMEOUT_SECONDS:-2}

respond() {
  status=$1
  reason=$2
  body=$3
  printf 'HTTP/1.1 %s %s\r\n' "$status" "$reason"
  printf 'Content-Type: application/json\r\n'
  printf 'Content-Length: %s\r\n' "${#body}"
  printf 'Connection: close\r\n\r\n'
  printf '%s' "$body"
  exit 0
}

valid_ipv4() {
  candidate=$1
  case "$candidate" in
    *[!0-9.]*|.*|*.|*..*) return 1 ;;
  esac
  old_ifs=$IFS
  IFS=.
  set -- $candidate
  IFS=$old_ifs
  [ "$#" -eq 4 ] || return 1
  for octet in "$@"; do
    [ -n "$octet" ] && [ "$octet" -ge 0 ] && [ "$octet" -le 255 ] || return 1
  done
}

IFS=' ' read -r -t 2 -n 513 method request http_version || exit 0
case "$http_version" in
  HTTP/1.0*|HTTP/1.1*) ;;
  *) respond 400 'Bad Request' '{"status":"invalid-http-version"}' ;;
esac
[ "$method" = GET ] || respond 405 'Method Not Allowed' '{"status":"method-not-allowed"}'
[ "${#request}" -le 512 ] || respond 414 'URI Too Long' '{"status":"uri-too-long"}'

if [ "$request" = /health ]; then
  respond 200 OK "{\"status\":\"healthy\",\"candidate_sha\":\"$OJOS_CAPACITY_CANDIDATE_SHA\",\"service_id\":\"$OJOS_CAPACITY_SERVICE_ID\"}"
fi

case "$request" in
  /probe?target=*) encoded_target=${request#'/probe?target='} ;;
  *) respond 404 'Not Found' '{"status":"not-found"}' ;;
esac

# Capacity endpoints contain only an IPv4 literal, decimal port and lowercase
# service id. A form encoder changes only ':' to %3A for that alphabet. Decode
# precisely that sequence and reject every other escape instead of accepting a
# general-purpose URL/SSRF input.
target=$(printf '%s' "$encoded_target" | sed 's/%3[Aa]/:/g')
case "$target" in
  *%*|*+*|*[!0-9a-z.:-]*) respond 400 'Bad Request' '{"status":"invalid-target"}' ;;
esac
old_ifs=$IFS
IFS=:
set -- $target
IFS=$old_ifs
[ "$#" -eq 3 ] || respond 400 'Bad Request' '{"status":"invalid-target"}'
target_host=$1
target_port=$2
target_service=$3
valid_ipv4 "$target_host" || respond 400 'Bad Request' '{"status":"invalid-target-host"}'
case "$target_port" in
  ''|*[!0-9]*) respond 400 'Bad Request' '{"status":"invalid-target-port"}' ;;
esac
[ "$target_port" -ge "$MIN_PROBE_PORT" ] && [ "$target_port" -le "$MAX_PROBE_PORT" ] \
  || respond 403 Forbidden '{"status":"target-port-not-allowed"}'
case "$target_service" in
  capacity-[0-9][0-9]) ;;
  *) respond 403 Forbidden '{"status":"target-service-not-allowed"}' ;;
esac

probe_file="/tmp/ojos-capacity-probe.$$"
trap 'rm -f "$probe_file"' EXIT HUP INT TERM
# Use a raw, numeric-address HTTP/1.1 exchange. It cannot follow redirects,
# resolve DNS or change the fixed /health path. RLIMIT_FSIZE is expressed in
# 512-byte blocks by the pinned Alpine BusyBox ash, so 8 is a hard 4096-byte
# cap covering headers and body; a larger response terminates nc with SIGXFSZ.
if ! (
  ulimit -f 8
  printf 'GET /health HTTP/1.1\r\nHost: %s:%s\r\nConnection: close\r\n\r\n' \
    "$target_host" "$target_port" \
    | busybox nc -n -w "$PROBE_TIMEOUT_SECONDS" "$target_host" "$target_port" \
      >"$probe_file"
); then
  respond 502 'Bad Gateway' '{"status":"target-unreachable"}'
fi
probe_size=$(wc -c <"$probe_file")
[ "$probe_size" -le "$MAX_RESPONSE_BYTES" ] \
  || respond 502 'Bad Gateway' '{"status":"target-response-too-large"}'
IFS= read -r target_status <"$probe_file" || respond 502 'Bad Gateway' '{"status":"target-empty-response"}'
case "$target_status" in
  'HTTP/1.0 200 '*|'HTTP/1.1 200 '*) ;;
  *) respond 502 'Bad Gateway' '{"status":"target-health-not-ok"}' ;;
esac
target_body=$(cat "$probe_file")
case "$target_body" in
  *"\"status\":\"healthy\""*"\"candidate_sha\":\"$OJOS_CAPACITY_CANDIDATE_SHA\""*"\"service_id\":\"$target_service\""*) ;;
  *) respond 502 'Bad Gateway' '{"status":"target-identity-mismatch"}' ;;
esac

respond 200 OK "{\"status\":\"healthy\",\"candidate_sha\":\"$OJOS_CAPACITY_CANDIDATE_SHA\",\"source_service_id\":\"$OJOS_CAPACITY_SERVICE_ID\",\"target_endpoint\":\"$target\",\"target_service_id\":\"$target_service\"}"
