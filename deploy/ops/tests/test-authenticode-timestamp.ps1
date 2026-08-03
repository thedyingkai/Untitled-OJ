[CmdletBinding()]
param()

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

. (Join-Path $PSScriptRoot "../../release/authenticode-timestamp.ps1")

function Join-Bytes {
    param([Parameter(Mandatory = $true)][object[]]$Parts)
    $result = [Collections.Generic.List[byte]]::new()
    foreach ($part in $Parts) {
        $result.AddRange([byte[]]$part)
    }
    return $result.ToArray()
}

function New-DerElement {
    param(
        [Parameter(Mandatory = $true)][byte]$Tag,
        [Parameter(Mandatory = $true)][byte[]]$Content
    )
    if ($Content.Length -ge 128) {
        throw "test fixture only supports short DER lengths"
    }
    return Join-Bytes -Parts @(
        [byte[]]@($Tag, [byte]$Content.Length),
        $Content
    )
}

function New-TestTstInfo {
    param(
        [Parameter(Mandatory = $true)][byte[]]$DigestOidValue,
        [Parameter(Mandatory = $true)][byte[]]$MessageImprint
    )
    $algorithm = New-DerElement -Tag 0x30 -Content (Join-Bytes -Parts @(
        (New-DerElement -Tag 0x06 -Content $DigestOidValue),
        ([byte[]]@(0x05, 0x00))
    ))
    $messageImprint = New-DerElement -Tag 0x30 -Content (Join-Bytes -Parts @(
        $algorithm,
        (New-DerElement -Tag 0x04 -Content $MessageImprint)
    ))
    $generalizedTime = [Text.Encoding]::ASCII.GetBytes("20260101000000Z")
    return New-DerElement -Tag 0x30 -Content (Join-Bytes -Parts @(
        (New-DerElement -Tag 0x02 -Content ([byte[]]@(1))),
        (New-DerElement -Tag 0x06 -Content ([byte[]]@(0x2a, 0x03))),
        $messageImprint,
        (New-DerElement -Tag 0x02 -Content ([byte[]]@(1))),
        (New-DerElement -Tag 0x18 -Content $generalizedTime)
    ))
}

$parentSignature = [byte[]]@(1, 3, 3, 7, 9, 11, 13, 17)
$sha256Algorithm = [Security.Cryptography.SHA256]::Create()
try {
    $sha256Imprint = $sha256Algorithm.ComputeHash($parentSignature)
}
finally {
    $sha256Algorithm.Dispose()
}
$sha256 = New-TestTstInfo `
    -DigestOidValue ([byte[]]@(0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02, 0x01)) `
    -MessageImprint $sha256Imprint
$sha256Info = Read-Rfc3161TstInfo -Bytes $sha256
Assert-Rfc3161Sha256TimestampInfo -TimestampInfo $sha256Info -Location "SHA256 fixture"
Assert-Rfc3161MessageImprint `
    -TimestampInfo $sha256Info `
    -ParentSignatureBytes $parentSignature `
    -Location "SHA256 fixture"
if ($sha256Info.DigestOid -ne "2.16.840.1.101.3.4.2.1" -or
    $sha256Info.MessageImprintLength -ne 32) {
    throw "SHA256 TSTInfo fixture was parsed incorrectly"
}

$sha1Algorithm = [Security.Cryptography.SHA1]::Create()
try {
    $sha1Imprint = $sha1Algorithm.ComputeHash($parentSignature)
}
finally {
    $sha1Algorithm.Dispose()
}
$sha1 = New-TestTstInfo `
    -DigestOidValue ([byte[]]@(0x2b, 0x0e, 0x03, 0x02, 0x1a)) `
    -MessageImprint $sha1Imprint
$sha1Info = Read-Rfc3161TstInfo -Bytes $sha1
$sha1Rejected = $false
try {
    Assert-Rfc3161Sha256TimestampInfo -TimestampInfo $sha1Info -Location "SHA1 fixture"
}
catch {
    $sha1Rejected = $true
}
if (-not $sha1Rejected) {
    throw "an RFC3161 SHA1 timestamp fixture was accepted as SHA256"
}

$unrelatedTokenRejected = $false
try {
    Assert-Rfc3161MessageImprint `
        -TimestampInfo $sha256Info `
        -ParentSignatureBytes ([byte[]]@(99, 98, 97)) `
        -Location "unrelated token fixture"
}
catch {
    $unrelatedTokenRejected = $true
}
if (-not $unrelatedTokenRejected) {
    throw "an unrelated RFC3161 token with the right algorithm was accepted"
}

$legacy = Resolve-AuthenticodeTimestampProtocol `
    -UnsignedAttributeOids @("1.2.840.113549.1.9.6")
if ($legacy -ne "AuthenticodeLegacy") {
    throw "legacy Authenticode timestamp fixture was not classified honestly"
}
$legacyRejected = $false
try {
    Assert-Rfc3161Sha256TimestampInfo -TimestampInfo ([pscustomobject]@{
        Protocol = $legacy
        ContentTypeOid = $null
        DigestOid = $null
        DigestAlgorithm = "UNKNOWN"
        MessageImprintLength = 0
    }) -Location "legacy fixture"
}
catch {
    $legacyRejected = $true
}
if (-not $legacyRejected) {
    throw "a legacy Authenticode timestamp fixture was accepted as RFC3161"
}

$none = Resolve-AuthenticodeTimestampProtocol -UnsignedAttributeOids @()
if ($none -ne "None") {
    throw "a signature without a timestamp was not classified honestly"
}
$noneRejected = $false
try {
    Assert-Rfc3161Sha256TimestampInfo -TimestampInfo ([pscustomobject]@{
        Protocol = $none
        ContentTypeOid = $null
        DigestOid = $null
        DigestAlgorithm = "NONE"
        MessageImprintLength = 0
    }) -Location "no timestamp fixture"
}
catch {
    $noneRejected = $true
}
if (-not $noneRejected) {
    throw "a missing timestamp was accepted as RFC3161"
}

$ambiguousRejected = $false
try {
    Resolve-AuthenticodeTimestampProtocol `
        -UnsignedAttributeOids @(
            "1.3.6.1.4.1.311.3.3.1",
            "1.2.840.113549.1.9.6"
        ) | Out-Null
}
catch {
    $ambiguousRejected = $true
}
if (-not $ambiguousRejected) {
    throw "ambiguous timestamp protocols were accepted"
}

$truncatedRejected = $false
try {
    Read-Rfc3161TstInfo -Bytes ([byte[]]$sha256[0..($sha256.Length - 2)]) | Out-Null
}
catch {
    $truncatedRejected = $true
}
if (-not $truncatedRejected) {
    throw "truncated RFC3161 TSTInfo was accepted"
}

$moduleText = Get-Content `
    -LiteralPath (Join-Path $PSScriptRoot "../../release/authenticode-timestamp.ps1") `
    -Raw
if ($moduleText -notmatch [regex]::Escape('$timestampToken.CheckSignature($true)')) {
    throw "RFC3161 token cryptographic signature verification is missing"
}

Write-Host "structured RFC3161/SHA256 timestamp parser fixtures passed"
