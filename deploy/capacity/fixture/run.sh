#!/bin/sh
set -eu

test -n "${OJOS_CAPACITY_CANDIDATE_SHA:-}"
test -n "${OJOS_CAPACITY_SERVICE_ID:-}"
printf '%s\n' "$OJOS_CAPACITY_CANDIDATE_SHA" >/tmp/ojos-capacity-candidate
printf '%s\n' "$OJOS_CAPACITY_SERVICE_ID" >/tmp/ojos-capacity-service
case "$OJOS_CAPACITY_CANDIDATE_SHA" in
  *[!0-9a-f]*|'') exit 64 ;;
esac
[ "${#OJOS_CAPACITY_CANDIDATE_SHA}" -eq 40 ]
case "$OJOS_CAPACITY_SERVICE_ID" in
  capacity-[0-9][0-9]) ;;
  *) exit 64 ;;
esac

exec busybox nc -lk -p 8080 -e /usr/local/bin/ojos-capacity-http-handler
