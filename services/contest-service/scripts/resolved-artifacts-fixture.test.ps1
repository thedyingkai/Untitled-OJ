$ErrorActionPreference = 'Stop'
$script = Join-Path $PSScriptRoot 'resolved-artifacts-fixture.ps1'
$first = Join-Path ([IO.Path]::GetTempPath()) ("ojos-contest-artifacts-{0}.json" -f [Guid]::NewGuid())
$second = Join-Path ([IO.Path]::GetTempPath()) ("ojos-contest-artifacts-{0}.json" -f [Guid]::NewGuid())
try {
    & $script -Output $first | Out-Null
    & $script -Output $second | Out-Null
    $firstHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $first).Hash
    $secondHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $second).Hash
    if ($firstHash -ne $secondHash) {
        throw 'fixture generation is not deterministic'
    }
    $document = Get-Content -Raw -LiteralPath $first | ConvertFrom-Json
    if ($document.schemaVersion -ne 'ojos.dev/resolved-artifacts/v1') {
        throw 'fixture schemaVersion is invalid'
    }
    foreach ($slot in @('contest-runtime', 'contest-migration-v1')) {
        $artifact = $document.artifacts.$slot
        if ($artifact.reference -notmatch '^example\.invalid/ojos/.+@sha256:[a-f0-9]{64}$') {
            throw "fixture OCI slot $slot does not have a deterministic digest-pinned reference"
        }
    }
    foreach ($slot in @('contest-user-frontend', 'contest-admin-frontend')) {
        $artifact = $document.artifacts.$slot
        if ($artifact.reference -notmatch '^https://fixture\.invalid/__ojos/extensions/[a-f0-9]{64}/bundle\.js$') {
            throw "fixture frontend slot $slot does not have a content-addressed HTTPS reference"
        }
    }
    foreach ($slot in @('contract', 'openapi.contest-service.api')) {
        $artifact = $document.artifacts.$slot
        if ($artifact.reference -notmatch '^https://fixture\.invalid/__ojos/artifacts/[a-f0-9]{64}/') {
            throw "fixture source slot $slot is not clearly content-addressed under fixture.invalid"
        }
    }
    foreach ($slot in @('sbom', 'provenance', 'events')) {
        $artifact = $document.artifacts.$slot
        if ($artifact.reference -notmatch '^https://fixture\.invalid/__ojos/artifacts/[a-f0-9]{64}/') {
            throw "fixture evidence slot $slot is not clearly content-addressed under fixture.invalid"
        }
    }
} finally {
    Remove-Item -Force -ErrorAction SilentlyContinue -LiteralPath $first, $second
}
