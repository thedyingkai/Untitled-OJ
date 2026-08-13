param(
    [string]$Output = (Join-Path $PSScriptRoot '..\build\resolved-artifacts.fixture.json')
)

$ErrorActionPreference = 'Stop'
$serviceRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$buildInput = Get-Content -Raw -LiteralPath (Join-Path $serviceRoot 'gen\build-input.json') | ConvertFrom-Json
$rawSourceBySlot = @{
    'contract' = Join-Path $serviceRoot 'gen\service.contract.json'
    'config.schema' = Join-Path $serviceRoot 'config.schema.json'
    'openapi.gateway.platform.api' = Join-Path $serviceRoot 'api\openapi.yaml'
}

$runtimePayload = [Text.Encoding]::UTF8.GetBytes('fixture:gateway-runtime:0.1.0')
$runtimeSHA = [Security.Cryptography.SHA256]::Create()
try {
    $runtimeHash = -join ($runtimeSHA.ComputeHash($runtimePayload) | ForEach-Object { $_.ToString('x2') })
} finally {
    $runtimeSHA.Dispose()
}
$artifacts = [ordered]@{
    'gateway-runtime' = [ordered]@{
        mediaType = 'application/vnd.oci.image.manifest.v1+json'
        digest = "sha256:$runtimeHash"
        size = $runtimePayload.Length
        reference = "example.invalid/ojos/gateway-runtime@sha256:$runtimeHash"
    }
}

foreach ($requirement in $buildInput.artifactRequirements) {
    if (-not $rawSourceBySlot.ContainsKey($requirement.slot)) { continue }
    $source = $rawSourceBySlot[$requirement.slot]
    $hash = (Get-FileHash -Algorithm SHA256 -LiteralPath $source).Hash.ToLowerInvariant()
    if (-not [string]::IsNullOrWhiteSpace($requirement.expectedDigest)) {
        $hash = $requirement.expectedDigest.Substring('sha256:'.Length)
    }
    $artifacts[$requirement.slot] = [ordered]@{
        mediaType = 'application/vnd.ojos.fixture+octet-stream'
        digest = "sha256:$hash"
        size = (Get-Item -LiteralPath $source).Length
        reference = "https://fixture.invalid/__ojos/artifacts/$hash/$($requirement.slot)"
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
    if (-not $artifacts.Contains($requirement.slot)) {
        throw "fixture did not resolve required slot $($requirement.slot)"
    }
}
$document = [ordered]@{ schemaVersion = 'ojos.dev/resolved-artifacts/v1'; artifacts = $artifacts }
$parent = Split-Path -Parent $Output
New-Item -ItemType Directory -Force -Path $parent | Out-Null
[IO.File]::WriteAllText($Output, ($document | ConvertTo-Json -Depth 8) + [Environment]::NewLine, [Text.UTF8Encoding]::new($false))
Write-Output $Output
