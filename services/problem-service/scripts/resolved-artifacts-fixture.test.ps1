$ErrorActionPreference = 'Stop'
$script = Join-Path $PSScriptRoot 'resolved-artifacts-fixture.ps1'
$first = Join-Path ([IO.Path]::GetTempPath()) ("ojos-problem-artifacts-{0}.json" -f [Guid]::NewGuid())
$second = Join-Path ([IO.Path]::GetTempPath()) ("ojos-problem-artifacts-{0}.json" -f [Guid]::NewGuid())
try {
    & $script -Output $first | Out-Null
    & $script -Output $second | Out-Null
    if ((Get-FileHash -Algorithm SHA256 -LiteralPath $first).Hash -ne (Get-FileHash -Algorithm SHA256 -LiteralPath $second).Hash) {
        throw 'fixture generation is not deterministic'
    }
    $document = Get-Content -Raw -LiteralPath $first | ConvertFrom-Json
    if ($document.schemaVersion -ne 'ojos.dev/resolved-artifacts/v1') { throw 'fixture schemaVersion is invalid' }
    foreach ($slot in @('problem-runtime', 'problem-migration-v1')) {
        if ($document.artifacts.$slot.reference -notmatch '^example\.invalid/ojos/.+@sha256:[a-f0-9]{64}$') { throw "fixture OCI slot $slot is not digest-pinned" }
    }
    foreach ($slot in @('problem-user-frontend', 'problem-admin-frontend')) {
        if ($document.artifacts.$slot.reference -notmatch '^https://fixture\.invalid/__ojos/extensions/[a-f0-9]{64}/bundle\.js$') { throw "fixture frontend slot $slot is not content-addressed" }
    }
    foreach ($slot in @('contract', 'openapi.problem.problem.read', 'openapi.problem.problem.manage', 'openapi.problem.testdata.read', 'openapi.problem.testdata.write', 'openapi.problem.artifact-gc.manage', 'sbom', 'provenance', 'events')) {
        if ($document.artifacts.$slot.reference -notmatch '^https://fixture\.invalid/__ojos/artifacts/[a-f0-9]{64}/') { throw "fixture slot $slot is not content-addressed" }
    }
} finally {
    Remove-Item -Force -ErrorAction SilentlyContinue -LiteralPath $first, $second
}
