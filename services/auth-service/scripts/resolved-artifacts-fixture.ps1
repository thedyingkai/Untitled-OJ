param(
    [string]$Output = (Join-Path $PSScriptRoot '..\build\resolved-artifacts.fixture.json')
)

$ErrorActionPreference = 'Stop'
$serviceRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$buildInput = Get-Content -Raw -LiteralPath (Join-Path $serviceRoot 'gen\build-input.json') | ConvertFrom-Json

$rawSourceBySlot = @{
    'contract' = Join-Path $serviceRoot 'gen\service.contract.json'
    'openapi.auth-service.api' = Join-Path $serviceRoot 'api\openapi.yaml'
    'openapi.auth.user.permission.check' = Join-Path $serviceRoot 'api\permission-check.openapi.yaml'
    'openapi.auth.control.v1' = Join-Path $serviceRoot 'api\control.openapi.yaml'
}
$fixturePayloadBySlot = [ordered]@{
    'auth-runtime' = [Text.Encoding]::UTF8.GetBytes('fixture:auth-runtime:0.1.0')
    'auth-migration-v1' = [Text.Encoding]::UTF8.GetBytes('fixture:auth-migration-v1:0.1.0')
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
    if ($rawSourceBySlot.ContainsKey($requirement.slot)) {
        $source = $rawSourceBySlot[$requirement.slot]
        $hash = (Get-FileHash -Algorithm SHA256 -LiteralPath $source).Hash.ToLowerInvariant()
        $artifacts[$requirement.slot] = [ordered]@{
            mediaType = 'application/vnd.ojos.fixture+octet-stream'
            digest = "sha256:$hash"
            size = (Get-Item -LiteralPath $source).Length
            reference = "https://fixture.invalid/__ojos/artifacts/$hash/$($requirement.slot)"
        }
        continue
    }
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
