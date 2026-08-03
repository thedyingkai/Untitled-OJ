#!/usr/bin/env bash
set -Eeuo pipefail

payload_dir="${1:?payload directory is required}"
manifest="${2:?candidate manifest is required}"
candidate_sha="${3:?candidate SHA is required}"
candidate_run_id="${4:?candidate workflow run id is required}"
repository="${5:?owner/repository is required}"
version="${6:-1.0.0}"
candidate_run_attempt="${7:?candidate workflow run attempt is required}"
workflow_name="${8:-Orchestrator v1 signed candidate}"
workflow_ref="refs/heads/main"
workflow_identity="https://github.com/$repository/.github/workflows/release.yml@$workflow_ref"
script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
candidate_tool="$script_dir/orchestrator-candidate.py"

[[ "$candidate_sha" =~ ^[0-9a-f]{40}$ ]] || {
  echo "candidate SHA must be canonical lowercase hexadecimal" >&2
  exit 1
}
[[ "$candidate_run_attempt" == "1" ]] || {
  echo "candidate trust verification refuses workflow reruns" >&2
  exit 1
}
for command_name in python3 cosign gh sha256sum; do
  command -v "$command_name" >/dev/null 2>&1 || {
    echo "$command_name is required for candidate trust verification" >&2
    exit 1
  }
done

python3 "$candidate_tool" verify \
  --payload-dir "$payload_dir" \
  --manifest "$manifest" \
  --expected-sha "$candidate_sha" \
  --expected-run-id "$candidate_run_id" \
  --expected-run-attempt "$candidate_run_attempt" \
  --expected-repository "$repository" \
  --expected-workflow-ref "$workflow_ref"

mapfile -t primary < <(
  python3 "$candidate_tool" list-assets --version "$version"
)
[[ "${#primary[@]}" -eq 11 ]] || exit 1
trust_output="$(mktemp -d)"
trap 'rm -rf -- "$trust_output"' EXIT
index=0
for name in "${primary[@]}"; do
  artifact="$payload_dir/$name"
  bundle="$artifact.sigstore.json"
  cosign verify-blob \
    --bundle "$bundle" \
    --certificate-identity "$workflow_identity" \
    --certificate-oidc-issuer https://token.actions.githubusercontent.com \
    --certificate-github-workflow-name "$workflow_name" \
    --certificate-github-workflow-repository "$repository" \
    --certificate-github-workflow-ref "$workflow_ref" \
    --certificate-github-workflow-sha "$candidate_sha" \
    --certificate-github-workflow-trigger workflow_dispatch \
    "$artifact" >/dev/null
  attestation_json="$trust_output/attestation-$index.json"
  gh attestation verify "$artifact" \
    --repo "$repository" \
    --cert-identity "$workflow_identity" \
    --cert-oidc-issuer https://token.actions.githubusercontent.com \
    --signer-digest "$candidate_sha" \
    --source-digest "$candidate_sha" \
    --source-ref "$workflow_ref" \
    --predicate-type https://slsa.dev/provenance/v1 \
    --deny-self-hosted-runners \
    --format json >"$attestation_json"
  python3 - "$attestation_json" <<'PY'
import json
import pathlib
import sys

value = json.loads(pathlib.Path(sys.argv[1]).read_text(encoding="utf-8"))
if not isinstance(value, list) or not value:
    raise SystemExit("GitHub attestation verification returned no verified statements")
PY
  index=$((index + 1))
done

(
  cd "$payload_dir"
  sha256sum -c "ojos-orchestrator-$version-ga-windows-x64.SHA256SUMS"
  sha256sum -c "ojos-orchestrator-$version-ga-linux-x86_64.SHA256SUMS"
)
echo "all 11 primary artifacts passed Sigstore, GitHub attestation and checksum verification"
