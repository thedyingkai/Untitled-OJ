#!/bin/sh
set -eu

printf '%s\n' "${OJOS_E2E_VERSION:?missing OJOS_E2E_VERSION}" >/tmp/ojos-e2e-version
touch /tmp/ojos-e2e-healthy
trap 'rm -f /tmp/ojos-e2e-healthy; exit 0' TERM INT

while :; do
  sleep 1
done
