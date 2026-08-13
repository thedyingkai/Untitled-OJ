param(
    [string]$Output = (Join-Path $PSScriptRoot '..\build\resolved-artifacts.fixture.json')
)

$ErrorActionPreference = 'Stop'
$serviceRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$buildInputPath = Join-Path $serviceRoot 'gen\build-input.json'
$buildInput = Get-Content -Raw -LiteralPath $buildInputPath | ConvertFrom-Json

$rawSourceBySlot = @{
    'contract' = Join-Path $serviceRoot 'gen\service.contract.json'
    'openapi.contest-service.api' = Join-Path $serviceRoot 'api\openapi.yaml'
    'contest-user-frontend' = Join-Path $serviceRoot 'frontend\user\bundle.js'
    'contest-admin-frontend' = Join-Path $serviceRoot 'frontend\admin\bundle.js'
}

$fixturePayloadBySlot = [ordered]@{
    'contest-runtime' = [Text.Encoding]::UTF8.GetBytes('fixture:contest-runtime:0.1.0')
    'contest-migration-v1' = [Text.Encoding]::UTF8.GetBytes('fixture:contest-migration-v1:0.1.0')
}

$artifacts = [ordered]@{}
foreach ($slot in @('contest-runtime', 'contest-migration-v1')) {
    $payload = $fixturePayloadBySlot[$slot]
    $sha = [Security.Cryptography.SHA256]::Create()
    try {
        $hash = -join ($sha.ComputeHash($payload) | ForEach-Object { $_.ToString('x2') })
    } finally {
        $sha.Dispose()
    }
    $artifacts[$slot] = [ordered]@{
        mediaType = 'application/vnd.oci.image.manifest.v1+json'
        digest = "sha256:$hash"
        size = $payload.Length
        reference = "example.invalid/ojos/$slot@sha256:$hash"
    }
}
foreach ($requirement in $buildInput.artifactRequirements) {
    if (-not $rawSourceBySlot.ContainsKey($requirement.slot)) {
        continue
    }
    $source = $rawSourceBySlot[$requirement.slot]
    $hash = (Get-FileHash -Algorithm SHA256 -LiteralPath $source).Hash.ToLowerInvariant()
    $size = (Get-Item -LiteralPath $source).Length
    $artifacts[$requirement.slot] = [ordered]@{
        mediaType = 'application/vnd.ojos.fixture+octet-stream'
        digest = "sha256:$hash"
        size = $size
        reference = "https://fixture.invalid/__ojos/artifacts/$hash/$($requirement.slot)"
    }
    if ($requirement.slot -in @('contest-user-frontend', 'contest-admin-frontend')) {
        $artifacts[$requirement.slot].reference = "https://fixture.invalid/__ojos/extensions/$hash/bundle.js"
    }
}

foreach ($slot in @('contest-runtime', 'contest-migration-v1', 'contest-user-frontend', 'contest-admin-frontend')) {
    if (-not $artifacts.Contains($slot)) {
        throw "build input does not require executable/served fixture slot $slot"
    }
}

# Source-owned and evidence fixture subjects are deliberately resolved to
# digest-addressed HTTPS objects. These are shape-test references only.
foreach ($requirement in $buildInput.artifactRequirements) {
    if ($artifacts.Contains($requirement.slot)) {
        continue
    }
    $digest = $requirement.expectedDigest
    $size = $requirement.expectedSize
    if ([string]::IsNullOrWhiteSpace($digest)) {
        $payload = [Text.Encoding]::UTF8.GetBytes("fixture:$($requirement.slot):0.1.0")
        $sha = [Security.Cryptography.SHA256]::Create()
        try {
            $hex = -join ($sha.ComputeHash($payload) | ForEach-Object { $_.ToString('x2') })
        } finally {
            $sha.Dispose()
        }
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

$document = [ordered]@{
    schemaVersion = 'ojos.dev/resolved-artifacts/v1'
    artifacts = $artifacts
}

$parent = Split-Path -Parent $Output
New-Item -ItemType Directory -Force -Path $parent | Out-Null
$json = $document | ConvertTo-Json -Depth 8
[IO.File]::WriteAllText($Output, $json + [Environment]::NewLine, [Text.UTF8Encoding]::new($false))
Write-Output $Output
