#!/usr/bin/env bash
# The v1 backup format retains its historical "minio" field/path. Normalize only
# transport configuration; do not rewrite archived manifests or mix credentials.
normalize_s3_environment() {
  [[ -n "${S3_ENDPOINT:-}" ]] || return 0
  [[ -n "${S3_ACCESS_KEY:-}" && -n "${S3_SECRET_KEY:-}" ]] || \
    die "S3_ENDPOINT requires S3_ACCESS_KEY and S3_SECRET_KEY"
  [[ "${S3_USE_SSL:-true}" == true || "${S3_USE_SSL:-true}" == false ]] || \
    die "S3_USE_SSL must be true or false"
  MINIO_ENDPOINT="$S3_ENDPOINT"
  MINIO_ACCESS_KEY="$S3_ACCESS_KEY"
  MINIO_SECRET_KEY="$S3_SECRET_KEY"
  MINIO_USE_SSL="${S3_USE_SSL:-true}"
}
