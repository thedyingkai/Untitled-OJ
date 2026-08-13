param(
    [string]$Output = (Join-Path $PSScriptRoot '..\build\resolved-artifacts.fixture.json')
)

$ErrorActionPreference = 'Stop'
$serviceRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$buildInput = Get-Content -Raw -LiteralPath (Join-Path $serviceRoot 'gen\build-input.json') | ConvertFrom-Json

$rawSourceBySlot = @{
    'contract' = Join-Path $serviceRoot 'gen\service.contract.json'
    'openapi.judge.submission.create' = Join-Path $serviceRoot 'api\submission-create.openapi.yaml'
    'openapi.judge.submission.result' = Join-Path $serviceRoot 'api\submission-result.openapi.yaml'
    'openapi.judge.worker.status' = Join-Path $serviceRoot 'api\worker-status.openapi.yaml'
    'openapi.judge.queue.status' = Join-Path $serviceRoot 'api\queue-status.openapi.yaml'
    'openapi.judge.worker.control' = Join-Path $serviceRoot 'api\worker-control.openapi.yaml'
    'event.io.ojos.problem.snapshot.v1.v1.schema' = Join-Path $serviceRoot 'events\problem-snapshot-v1.schema.json'
    'event.io.ojos.problem.deleted.v1.v1.schema' = Join-Path $serviceRoot 'events\problem-deleted-v1.schema.json'
    'judge-user-frontend' = Join-Path $serviceRoot 'frontend\user\bundle.js'
    'judge-admin-frontend' = Join-Path $serviceRoot 'frontend\admin\bundle.js'
}
$fixturePayloadBySlot = [ordered]@{
    'judge-runtime' = [Text.Encoding]::UTF8.GetBytes('fixture:judge-runtime:0.1.0')
    'judge-migration-v1' = [Text.Encoding]::UTF8.GetBytes('fixture:judge-migration-v1:0.1.0')
}

$artifacts = [ordered]@{}
foreach ($slot in $fixturePayloadBySlot.Keys) {
    $payload = $fixturePayloadBySlot[$slot]
    $sha = [Security.Cryptography.SHA256]::Create()
    try { $hash = -join ($sha.ComputeHash($payload) | ForEach-Object { $_.ToString('x2') }) } finally { $sha.Dispose() }
    $artifacts[$slot] = [ordered]@{
        mediaType = 'application/vnd.oci.image.manifest.v1+json'
        digest = "sha256:$hash"
        size = $payload.Length
        reference = "example.invalid/ojos/$slot@sha256:$hash"
    }
}

foreach ($requirement in $buildInput.artifactRequirements) {
    if (-not $rawSourceBySlot.ContainsKey($requirement.slot)) { continue }
    $source = $rawSourceBySlot[$requirement.slot]
    $hash = (Get-FileHash -Algorithm SHA256 -LiteralPath $source).Hash.ToLowerInvariant()
    if (-not [string]::IsNullOrWhiteSpace($requirement.expectedDigest)) {
        $hash = $requirement.expectedDigest.Substring('sha256:'.Length)
    }
    $reference = "https://fixture.invalid/__ojos/artifacts/$hash/$($requirement.slot)"
    if ($requirement.slot -in @('judge-user-frontend', 'judge-admin-frontend')) {
        $reference = "https://fixture.invalid/__ojos/extensions/$hash/bundle.js"
    }
    $artifacts[$requirement.slot] = [ordered]@{
        mediaType = 'application/vnd.ojos.fixture+octet-stream'
        digest = "sha256:$hash"
        size = (Get-Item -LiteralPath $source).Length
        reference = $reference
    }
}

foreach ($requirement in $buildInput.artifactRequirements) {
    if ($artifacts.Contains($requirement.slot)) { continue }
    $digest = $requirement.expectedDigest
    $size = $requirement.expectedSize
    if ([string]::IsNullOrWhiteSpace($digest)) {
        $payload = [Text.Encoding]::UTF8.GetBytes("fixture:$($requirement.slot):0.1.0")
        $sha = [Security.Cryptography.SHA256]::Create()
        try { $hex = -join ($sha.ComputeHash($payload) | ForEach-Object { $_.ToString('x2') }) } finally { $sha.Dispose() }
        $digest = "sha256:$hex"
        $size = $payload.Length
    } elseif ($null -eq $size) {
        $size = 1
    }
    $hexDigest = $digest.Substring('sha256:'.Length)
    $artifacts[$requirement.slot] = [ordered]@{
        mediaType = 'application/vnd.ojos.fixture+octet-stream'
        digest = $digest
        size = [int64]$size
        reference = "https://fixture.invalid/__ojos/artifacts/$hexDigest/$($requirement.slot)"
    }
}

foreach ($requirement in $buildInput.artifactRequirements) {
    if (-not $artifacts.Contains($requirement.slot)) { throw "fixture did not resolve required slot $($requirement.slot)" }
}

$document = [ordered]@{ schemaVersion = 'ojos.dev/resolved-artifacts/v1'; artifacts = $artifacts }
$parent = Split-Path -Parent $Output
New-Item -ItemType Directory -Force -Path $parent | Out-Null
[IO.File]::WriteAllText($Output, ($document | ConvertTo-Json -Depth 8) + [Environment]::NewLine, [Text.UTF8Encoding]::new($false))
Write-Output $Output
