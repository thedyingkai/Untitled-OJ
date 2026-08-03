[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$BinaryDirectory,
    [Parameter(Mandatory = $true)]
    [string]$MsiPath,
    [Parameter(Mandatory = $true)]
    [string]$PortableZip,
    [Parameter(Mandatory = $true)]
    [string]$ExpectedPublisherSubject,
    [Parameter(Mandatory = $true)]
    [string]$CandidateSha,
    [Parameter(Mandatory = $true)]
    [string]$EvidencePath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"
. (Join-Path $PSScriptRoot "authenticode-timestamp.ps1")

if ($CandidateSha -notmatch '^[0-9a-f]{40}$') {
    throw "CandidateSha must be a canonical 40-character commit"
}
if ([string]::IsNullOrWhiteSpace($ExpectedPublisherSubject)) {
    throw "ExpectedPublisherSubject is required"
}

$BinaryDirectory = (Resolve-Path -LiteralPath $BinaryDirectory).Path
$MsiPath = (Resolve-Path -LiteralPath $MsiPath).Path
$PortableZip = (Resolve-Path -LiteralPath $PortableZip).Path
$EvidencePath = [IO.Path]::GetFullPath($EvidencePath)
$expectedExecutables = @(
    "ojos-orchestrator-daemon.exe",
    "ojos-orchestrator-tui.exe",
    "ojos-orchestrator-agent.exe",
    "ojos-orchestrator-desktop.exe"
)

function Find-SignTool {
    $command = Get-Command signtool.exe -ErrorAction SilentlyContinue
    if ($null -ne $command) {
        return $command.Source
    }
    $kits = Join-Path ${env:ProgramFiles(x86)} "Windows Kits\10\bin"
    $matches = @(Get-ChildItem -LiteralPath $kits -Filter signtool.exe -Recurse -File |
        Where-Object { $_.DirectoryName -match '\\x64$' } |
        Sort-Object FullName -Descending)
    if ($matches.Count -eq 0) {
        throw "Windows SDK signtool.exe is required"
    }
    return $matches[0].FullName
}

$signTool = Find-SignTool
$evidence = [Collections.Generic.List[object]]::new()

function Get-TextSha256([string]$Text) {
    $algorithm = [Security.Cryptography.SHA256]::Create()
    try {
        $bytes = [Text.Encoding]::UTF8.GetBytes($Text)
        return ([Convert]::ToHexString($algorithm.ComputeHash($bytes))).ToLowerInvariant()
    }
    finally {
        $algorithm.Dispose()
    }
}

function Invoke-SignToolVerification([string]$Path, [string]$Location) {
    $output = @(& $signTool "verify" "/pa" "/all" "/v" $Path 2>&1)
    $exitCode = $LASTEXITCODE
    $output | ForEach-Object { Write-Host $_ }
    if ($exitCode -ne 0) {
        throw "signtool verification failed for $Location"
    }
    $text = ($output | Out-String)
    if ([string]::IsNullOrWhiteSpace($text)) {
        throw "signtool returned no verification evidence for $Location"
    }
    return Get-TextSha256 -Text $text
}

function Assert-SignedByPublisher([string]$Path, [string]$Location) {
    $resolved = (Resolve-Path -LiteralPath $Path).Path
    $signature = Get-AuthenticodeSignature -LiteralPath $resolved
    if ($signature.Status -ne [Management.Automation.SignatureStatus]::Valid) {
        throw "$Location is not Authenticode Valid: $($signature.Status) $($signature.StatusMessage)"
    }
    if ($null -eq $signature.SignerCertificate -or
        -not $signature.SignerCertificate.Subject.Equals(
            $ExpectedPublisherSubject,
            [StringComparison]::Ordinal
        )) {
        throw "$Location publisher subject does not equal the protected expected subject"
    }
    $timestamp = Get-AuthenticodeTimestampInfo `
        -Path $resolved `
        -SignerThumbprint $signature.SignerCertificate.Thumbprint
    Assert-Rfc3161Sha256TimestampInfo `
        -TimestampInfo $timestamp `
        -Location $Location
    $signToolOutputSha256 = Invoke-SignToolVerification -Path $resolved -Location $Location
    $evidence.Add([ordered]@{
        location = $Location
        file_name = [IO.Path]::GetFileName($resolved)
        sha256 = (Get-FileHash -Algorithm SHA256 -LiteralPath $resolved).Hash.ToLowerInvariant()
        status = $signature.Status.ToString()
        publisher_subject = $signature.SignerCertificate.Subject
        publisher_thumbprint = $signature.SignerCertificate.Thumbprint
        timestamp_subject = $timestamp.TimestampSubject
        timestamp_thumbprint = $timestamp.TimestampThumbprint
        timestamp_protocol = $timestamp.Protocol
        timestamp_content_type_oid = $timestamp.ContentTypeOid
        timestamp_digest_oid = $timestamp.DigestOid
        timestamp_digest_algorithm = $timestamp.DigestAlgorithm
        timestamp_message_imprint_length = $timestamp.MessageImprintLength
        timestamp_message_imprint = $timestamp.MessageImprintHex
        timestamp_token_signature_valid = $timestamp.TokenSignatureValid
        timestamp_parent_signature_digest_verified = $timestamp.ParentSignatureDigestVerified
        signtool_policy = "pa/all/v"
        signtool_output_sha256 = $signToolOutputSha256
    })
}

function Assert-MicrosoftWebViewLoader([string]$Path, [string]$Location) {
    $resolved = (Resolve-Path -LiteralPath $Path).Path
    $signature = Get-AuthenticodeSignature -LiteralPath $resolved
    if ($signature.Status -ne [Management.Automation.SignatureStatus]::Valid -or
        $null -eq $signature.SignerCertificate -or
        $signature.SignerCertificate.Subject -notmatch '(^|, )(CN|O)=Microsoft Corporation(,|$)') {
        throw "$Location does not retain a valid Microsoft Authenticode signature"
    }
    if ($signature.SignerCertificate.Subject.Equals(
        $ExpectedPublisherSubject,
        [StringComparison]::Ordinal
    )) {
        throw "$Location was re-signed with the OJOS publisher instead of retaining Microsoft's signature"
    }
    $timestamp = Get-AuthenticodeTimestampInfo `
        -Path $resolved `
        -SignerThumbprint $signature.SignerCertificate.Thumbprint
    $signToolOutputSha256 = Invoke-SignToolVerification -Path $resolved -Location $Location
    $evidence.Add([ordered]@{
        location = $Location
        file_name = [IO.Path]::GetFileName($resolved)
        sha256 = (Get-FileHash -Algorithm SHA256 -LiteralPath $resolved).Hash.ToLowerInvariant()
        status = $signature.Status.ToString()
        publisher_subject = $signature.SignerCertificate.Subject
        publisher_thumbprint = $signature.SignerCertificate.Thumbprint
        timestamp_subject = $timestamp.TimestampSubject
        timestamp_thumbprint = $timestamp.TimestampThumbprint
        timestamp_protocol = $timestamp.Protocol
        timestamp_content_type_oid = $timestamp.ContentTypeOid
        timestamp_digest_oid = $timestamp.DigestOid
        timestamp_digest_algorithm = $timestamp.DigestAlgorithm
        timestamp_message_imprint_length = $timestamp.MessageImprintLength
        timestamp_message_imprint = $timestamp.MessageImprintHex
        timestamp_token_signature_valid = $timestamp.TokenSignatureValid
        timestamp_parent_signature_digest_verified = $timestamp.ParentSignatureDigestVerified
        retained_vendor_signature = "Microsoft"
        signtool_policy = "pa/all/v"
        signtool_output_sha256 = $signToolOutputSha256
    })
}

foreach ($name in $expectedExecutables) {
    Assert-SignedByPublisher -Path (Join-Path $BinaryDirectory $name) -Location "build/$name"
}
Assert-MicrosoftWebViewLoader -Path (Join-Path $BinaryDirectory "WebView2Loader.dll") `
    -Location "build/WebView2Loader.dll"
Assert-SignedByPublisher -Path $MsiPath -Location "installer/$(Split-Path -Leaf $MsiPath)"

$tempRoot = [IO.Path]::GetFullPath([IO.Path]::GetTempPath())
$work = Join-Path $tempRoot ("ojos-authenticode-" + [Guid]::NewGuid().ToString("N"))
[void](New-Item -ItemType Directory -Path $work)
try {
    $portable = Join-Path $work "portable"
    $installed = Join-Path $work "msi"
    [void](New-Item -ItemType Directory -Path $portable)
    [void](New-Item -ItemType Directory -Path $installed)
    Expand-Archive -LiteralPath $PortableZip -DestinationPath $portable
    foreach ($name in $expectedExecutables) {
        $matches = @(Get-ChildItem -LiteralPath $portable -Recurse -File -Filter $name)
        if ($matches.Count -ne 1) {
            throw "portable ZIP must contain exactly one $name"
        }
        Assert-SignedByPublisher -Path $matches[0].FullName -Location "portable/$name"
    }
    $loaders = @(Get-ChildItem -LiteralPath $portable -Recurse -File -Filter WebView2Loader.dll)
    if ($loaders.Count -ne 1) {
        throw "portable ZIP must contain exactly one WebView2Loader.dll"
    }
    Assert-MicrosoftWebViewLoader -Path $loaders[0].FullName -Location "portable/WebView2Loader.dll"

    $msiexec = (Get-Command msiexec.exe -ErrorAction Stop).Source
    $startInfo = [Diagnostics.ProcessStartInfo]::new()
    $startInfo.FileName = $msiexec
    $startInfo.UseShellExecute = $false
    foreach ($argument in @("/a", $MsiPath, "/qn", "/norestart", "TARGETDIR=$installed")) {
        [void]$startInfo.ArgumentList.Add($argument)
    }
    $process = [Diagnostics.Process]::Start($startInfo)
    if ($null -eq $process) {
        throw "MSI administrative extraction process did not start"
    }
    $process.WaitForExit()
    if ($process.ExitCode -notin @(0, 3010)) {
        throw "MSI administrative extraction exited with $($process.ExitCode)"
    }
    $desktopName = "ojos-orchestrator-desktop.exe"
    $installedDesktops = @(
        Get-ChildItem -LiteralPath $installed -Recurse -File -Filter $desktopName
    )
    if ($installedDesktops.Count -ne 1) {
        throw "MSI must contain exactly one signed $desktopName"
    }
    Assert-SignedByPublisher -Path $installedDesktops[0].FullName `
        -Location "msi/$desktopName"
    $installedLoaders = @(
        Get-ChildItem -LiteralPath $installed -Recurse -File -Filter WebView2Loader.dll
    )
    if ($installedLoaders.Count -ne 1) {
        throw "MSI must contain exactly one WebView2Loader.dll"
    }
    Assert-MicrosoftWebViewLoader -Path $installedLoaders[0].FullName `
        -Location "msi/WebView2Loader.dll"
}
finally {
    $resolvedWork = [IO.Path]::GetFullPath($work)
    if ($resolvedWork.StartsWith($tempRoot, [StringComparison]::OrdinalIgnoreCase)) {
        Remove-Item -LiteralPath $resolvedWork -Recurse -Force -ErrorAction SilentlyContinue
    }
}

$uniqueLocations = @($evidence | ForEach-Object { $_.location } | Sort-Object -Unique)
if ($evidence.Count -ne 13 -or $uniqueLocations.Count -ne 13) {
    throw "Authenticode verification must produce exactly 13 unique packaged locations"
}

$parent = Split-Path -Parent $EvidencePath
if (-not (Test-Path -LiteralPath $parent -PathType Container)) {
    [void](New-Item -ItemType Directory -Path $parent)
}
[ordered]@{
    schema_version = 2
    candidate_sha = $CandidateSha
    expected_publisher_subject = $ExpectedPublisherSubject
    timestamp_policy = [ordered]@{
        ojos_publisher = "RFC3161/SHA256"
        retained_microsoft = "verify-original-and-report-protocol"
    }
    verified_at = [DateTimeOffset]::UtcNow.ToString("o")
    files = $evidence
} | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $EvidencePath -Encoding utf8NoBOM

Write-Host "Authenticode, publisher, timestamp and retained Microsoft signature verification passed"
