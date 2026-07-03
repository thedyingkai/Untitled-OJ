#!/bin/sh
# One-shot MinIO provisioning for the compose stack.
#
# Runs as a short-lived minio/mc container after MinIO is up. It:
#   1. creates the OJOS buckets (idempotent),
#   2. adds a scoped, least-privilege service user (MINIO_ACCESS_KEY/MINIO_SECRET_KEY),
#   3. attaches a policy limited to exactly those buckets and the object verbs the
#      storage-service uses (Get/Put/Delete/Stat/List), and
#   4. sets a lifecycle expiry on the artifact buckets.
#
# storage-service authenticates as this scoped user, not the MinIO root account, and
# self-creates buckets only when absent (BucketExists is checked first), so the scoped
# user never needs CreateBucket.
set -eu

alias_name="ojos"
endpoint="http://${MINIO_HOST:-minio}:${MINIO_PORT:-9000}"
root_user="${MINIO_ROOT_USER:?MINIO_ROOT_USER is required}"
root_password="${MINIO_ROOT_PASSWORD:?MINIO_ROOT_PASSWORD is required}"
service_access_key="${MINIO_ACCESS_KEY:?MINIO_ACCESS_KEY is required}"
service_secret_key="${MINIO_SECRET_KEY:?MINIO_SECRET_KEY is required}"
buckets="${OJOS_STORAGE_BUCKETS:-problems,submissions,judge-artifacts,avatars}"
# Buckets whose objects are safe to expire; defaults to the transient artifact bucket.
lifecycle_buckets="${OJOS_MINIO_LIFECYCLE_BUCKETS:-judge-artifacts,submissions}"
lifecycle_expire_days="${OJOS_MINIO_LIFECYCLE_EXPIRE_DAYS:-30}"
policy_name="ojos-storage-rw"

echo "minio-init: waiting for MinIO at ${endpoint}"
tries=0
until mc alias set "$alias_name" "$endpoint" "$root_user" "$root_password" >/dev/null 2>&1; do
  tries=$((tries + 1))
  if [ "$tries" -ge 60 ]; then
    echo "minio-init: MinIO did not become ready in time" >&2
    exit 1
  fi
  sleep 2
done
echo "minio-init: MinIO is ready"

# 1. Buckets (idempotent).
IFS=','
for bucket in $buckets; do
  [ -n "$bucket" ] || continue
  mc mb --ignore-existing "$alias_name/$bucket"
done
unset IFS

# 2. Scoped policy limited to the configured buckets.
policy_file="$(mktemp)"
resources=""
IFS=','
for bucket in $buckets; do
  [ -n "$bucket" ] || continue
  resources="${resources}\"arn:aws:s3:::${bucket}\",\"arn:aws:s3:::${bucket}/*\","
done
unset IFS
resources="${resources%,}"
cat >"$policy_file" <<POLICY
{
  "Version": "2012-10-17",
  "Statement": [
    {
      "Effect": "Allow",
      "Action": [
        "s3:GetObject",
        "s3:PutObject",
        "s3:DeleteObject",
        "s3:ListBucket",
        "s3:GetBucketLocation"
      ],
      "Resource": [${resources}]
    }
  ]
}
POLICY
mc admin policy create "$alias_name" "$policy_name" "$policy_file" || \
  mc admin policy update "$alias_name" "$policy_name" "$policy_file"
rm -f "$policy_file"

# 3. Scoped service user + policy attach (idempotent).
mc admin user add "$alias_name" "$service_access_key" "$service_secret_key" >/dev/null 2>&1 || \
  mc admin user add "$alias_name" "$service_access_key" "$service_secret_key"
mc admin policy attach "$alias_name" "$policy_name" --user "$service_access_key" >/dev/null 2>&1 || true

# 4. Lifecycle expiry on artifact buckets (idempotent per bucket).
IFS=','
for bucket in $lifecycle_buckets; do
  [ -n "$bucket" ] || continue
  mc ilm rule add --expire-days "$lifecycle_expire_days" "$alias_name/$bucket" >/dev/null 2>&1 || \
    echo "minio-init: lifecycle rule for $bucket already present or unsupported; continuing"
done
unset IFS

echo "minio-init: provisioning complete (user=$service_access_key policy=$policy_name)"
