$ErrorActionPreference = 'Stop'
$serviceRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$repoRoot = [IO.Path]::GetFullPath((Join-Path $serviceRoot '..\..'))
$scratch = Join-Path ([IO.Path]::GetTempPath()) ("ojos-judge-publish-{0}" -f [Guid]::NewGuid().ToString('N'))
$artifacts = Join-Path $scratch 'resolved-artifacts.json'
$signingKey = Join-Path $scratch 'test-ed25519-seed.txt'
$releaseLock = Join-Path $scratch 'release.lock.json'
$catalogOutput = Join-Path $scratch 'catalog'

New-Item -ItemType Directory -Path $scratch | Out-Null
try {
    & (Join-Path $PSScriptRoot 'resolved-artifacts-fixture.ps1') -Output $artifacts | Out-Null
    $seed = New-Object byte[] 32
    $rng = [Security.Cryptography.RandomNumberGenerator]::Create()
    try { $rng.GetBytes($seed) } finally { $rng.Dispose() }
    [IO.File]::WriteAllText($signingKey, [Convert]::ToBase64String($seed) + [Environment]::NewLine, [Text.UTF8Encoding]::new($false))

    Push-Location $repoRoot
    try {
        & cargo run -p ojos-service -- service publish `
            $serviceRoot\ojos.service.yaml `
            --artifacts $artifacts `
            --output $releaseLock `
            --catalog-output $catalogOutput `
            --signing-key-file $signingKey `
            --key-id judge-fixture-test-key `
            --catalog-id judge-fixture-test `
            --public-base-url https://fixture.invalid/catalog | Out-Null
        if ($LASTEXITCODE -ne 0) { throw "ojos service publish exited with code $LASTEXITCODE" }
    } finally { Pop-Location }

    $catalogPath = Join-Path $catalogOutput 'catalog.json'
    $trustPath = Join-Path $catalogOutput 'trust.json'
    $sourcePath = Join-Path $catalogOutput 'catalog-source.json'
    $metadataPath = Join-Path $catalogOutput 'metadata\judge-api-0.1.0.release.json'
    $publishedLockPath = Join-Path $catalogOutput 'metadata\judge-api-0.1.0.release.lock.json'
    foreach ($path in @($releaseLock, $catalogPath, $trustPath, $sourcePath, $metadataPath, $publishedLockPath)) {
        if (-not (Test-Path -LiteralPath $path -PathType Leaf)) { throw "publish fixture did not create $path" }
    }
    $catalog = Get-Content -Raw -LiteralPath $catalogPath | ConvertFrom-Json
    if ($catalog.schema_version -ne 2 -or $catalog.signatures.Count -ne 1) { throw 'published Catalog v2 shape or signature count is invalid' }
    if ($catalog.signatures[0].key_id -ne 'judge-fixture-test-key' -or $catalog.signatures[0].algorithm -ne 'Ed25519') { throw 'published Catalog signature identity is invalid' }
    $trust = Get-Content -Raw -LiteralPath $trustPath | ConvertFrom-Json
    if ([string]::IsNullOrWhiteSpace($trust.'judge-fixture-test-key')) { throw 'published trust document is missing the test verification key' }
    $source = Get-Content -Raw -LiteralPath $sourcePath | ConvertFrom-Json
    if ($source[0].url -ne 'https://fixture.invalid/catalog/catalog.json') { throw 'published Catalog source URL is not deterministic' }
    $metadataHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $metadataPath).Hash.ToLowerInvariant()
    if ($catalog.modules[0].releases[0].metadata.sha256 -ne "sha256:$metadataHash") { throw 'Catalog metadata digest does not bind the published release document' }
    if ((Get-FileHash -Algorithm SHA256 -LiteralPath $releaseLock).Hash -ne (Get-FileHash -Algorithm SHA256 -LiteralPath $publishedLockPath).Hash) { throw 'published Catalog release lock differs from the sealed release lock' }
} finally {
    $resolvedScratch = [IO.Path]::GetFullPath($scratch)
    $tempRoot = [IO.Path]::GetFullPath([IO.Path]::GetTempPath())
    if ($resolvedScratch.StartsWith($tempRoot, [StringComparison]::OrdinalIgnoreCase) -and (Split-Path -Leaf $resolvedScratch).StartsWith('ojos-judge-publish-')) {
        Remove-Item -Recurse -Force -ErrorAction SilentlyContinue -LiteralPath $resolvedScratch
    } else { throw "refusing to clean unexpected scratch path $resolvedScratch" }
}
