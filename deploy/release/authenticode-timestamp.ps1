Set-StrictMode -Version Latest

$script:Rfc3161CounterSignatureOid = "1.3.6.1.4.1.311.3.3.1"
$script:LegacyCounterSignatureOid = "1.2.840.113549.1.9.6"
$script:TstInfoContentTypeOid = "1.2.840.113549.1.9.16.1.4"
$script:Sha256Oid = "2.16.840.1.101.3.4.2.1"

if (-not ("Ojos.AuthenticodeNative" -as [type])) {
    Add-Type -TypeDefinition @'
using System;
using System.ComponentModel;
using System.Runtime.InteropServices;

namespace Ojos
{
    public static class AuthenticodeNative
    {
        private const int CertQueryObjectFile = 1;
        private const int CertQueryContentPkcs7SignedEmbed = 10;
        private const int CertQueryFormatBinary = 1;
        private const int CmsgEncodedMessage = 29;

        [DllImport("crypt32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
        private static extern bool CryptQueryObject(
            int objectType,
            [MarshalAs(UnmanagedType.LPWStr)] string obj,
            int expectedContentTypeFlags,
            int expectedFormatTypeFlags,
            int flags,
            out int messageAndCertificateEncodingType,
            out int contentType,
            out int formatType,
            out IntPtr certificateStore,
            out IntPtr message,
            out IntPtr context);

        [DllImport("crypt32.dll", SetLastError = true)]
        private static extern bool CryptMsgGetParam(
            IntPtr cryptMessage,
            int parameterType,
            int index,
            [Out] byte[] data,
            ref int dataLength);

        [DllImport("crypt32.dll")]
        private static extern bool CryptMsgClose(IntPtr cryptMessage);

        [DllImport("crypt32.dll")]
        private static extern bool CertCloseStore(IntPtr certificateStore, int flags);

        public static byte[] ReadEmbeddedSignedCms(string path)
        {
            int encodingType;
            int contentType;
            int formatType;
            IntPtr store;
            IntPtr message;
            IntPtr context;
            int contentFlags = 1 << CertQueryContentPkcs7SignedEmbed;
            int formatFlags = 1 << CertQueryFormatBinary;
            if (!CryptQueryObject(
                CertQueryObjectFile,
                path,
                contentFlags,
                formatFlags,
                0,
                out encodingType,
                out contentType,
                out formatType,
                out store,
                out message,
                out context))
            {
                throw new Win32Exception(Marshal.GetLastWin32Error(),
                    "CryptQueryObject could not read the embedded Authenticode signature");
            }

            try
            {
                if (contentType != CertQueryContentPkcs7SignedEmbed || message == IntPtr.Zero)
                {
                    throw new InvalidOperationException("file does not contain embedded PKCS#7 Authenticode data");
                }
                int length = 0;
                if (!CryptMsgGetParam(message, CmsgEncodedMessage, 0, null, ref length))
                {
                    throw new Win32Exception(Marshal.GetLastWin32Error(),
                        "CryptMsgGetParam could not size the Authenticode message");
                }
                byte[] data = new byte[length];
                if (!CryptMsgGetParam(message, CmsgEncodedMessage, 0, data, ref length))
                {
                    throw new Win32Exception(Marshal.GetLastWin32Error(),
                        "CryptMsgGetParam could not read the Authenticode message");
                }
                if (length != data.Length)
                {
                    Array.Resize(ref data, length);
                }
                return data;
            }
            finally
            {
                if (message != IntPtr.Zero)
                {
                    CryptMsgClose(message);
                }
                if (store != IntPtr.Zero)
                {
                    CertCloseStore(store, 0);
                }
            }
        }
    }
}
'@
}

function Read-DerElement {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)]
        [byte[]]$Bytes,
        [Parameter(Mandatory = $true)]
        [ref]$Offset
    )

    $start = [int]$Offset.Value
    if ($start -lt 0 -or $start -ge $Bytes.Length) {
        throw "DER element starts outside the input"
    }
    $tag = [int]$Bytes[$start]
    $cursor = $start + 1
    if ($cursor -ge $Bytes.Length) {
        throw "DER element has no length"
    }
    $firstLength = [int]$Bytes[$cursor]
    $cursor++
    if (($firstLength -band 0x80) -eq 0) {
        $length = $firstLength
    }
    else {
        $lengthBytes = $firstLength -band 0x7f
        if ($lengthBytes -eq 0 -or $lengthBytes -gt 4 -or
            $cursor + $lengthBytes -gt $Bytes.Length) {
            throw "DER element has an invalid length encoding"
        }
        $length = 0
        for ($index = 0; $index -lt $lengthBytes; $index++) {
            $length = ($length -shl 8) -bor [int]$Bytes[$cursor + $index]
        }
        if ($length -lt 128) {
            throw "DER element uses a non-minimal length encoding"
        }
        $cursor += $lengthBytes
    }
    $end = $cursor + $length
    if ($length -lt 0 -or $end -gt $Bytes.Length) {
        throw "DER element extends past the input"
    }
    $Offset.Value = $end
    return [pscustomobject]@{
        Tag = $tag
        Start = $start
        ContentOffset = $cursor
        ContentLength = $length
        End = $end
    }
}

function ConvertFrom-DerOid {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)]
        [byte[]]$Bytes,
        [Parameter(Mandatory = $true)]
        [int]$Offset,
        [Parameter(Mandatory = $true)]
        [int]$Length
    )

    if ($Length -le 0 -or $Offset -lt 0 -or $Offset + $Length -gt $Bytes.Length) {
        throw "DER object identifier is empty or truncated"
    }
    $first = [int]$Bytes[$Offset]
    if ($first -lt 40) {
        $arcs = [Collections.Generic.List[UInt64]]@([UInt64]0, [UInt64]$first)
    }
    elseif ($first -lt 80) {
        $arcs = [Collections.Generic.List[UInt64]]@([UInt64]1, [UInt64]($first - 40))
    }
    else {
        $arcs = [Collections.Generic.List[UInt64]]@([UInt64]2, [UInt64]($first - 80))
    }
    $value = [UInt64]0
    $openArc = $false
    for ($index = 1; $index -lt $Length; $index++) {
        $current = [int]$Bytes[$Offset + $index]
        if ($value -gt ([UInt64]::MaxValue -band 0xFFFFFFFFFFFFFF80) / 128) {
            throw "DER object identifier arc is too large"
        }
        $value = ($value * 128) + [UInt64]($current -band 0x7f)
        $openArc = $true
        if (($current -band 0x80) -eq 0) {
            $arcs.Add($value)
            $value = [UInt64]0
            $openArc = $false
        }
    }
    if ($openArc) {
        throw "DER object identifier has an unterminated arc"
    }
    return ($arcs | ForEach-Object { $_.ToString() }) -join "."
}

function Read-Rfc3161TstInfo {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)]
        [byte[]]$Bytes
    )

    $offset = 0
    $root = Read-DerElement -Bytes $Bytes -Offset ([ref]$offset)
    if ($root.Tag -ne 0x30 -or $root.End -ne $Bytes.Length) {
        throw "RFC3161 TSTInfo must be one complete DER sequence"
    }
    $cursor = $root.ContentOffset
    $version = Read-DerElement -Bytes $Bytes -Offset ([ref]$cursor)
    $policy = Read-DerElement -Bytes $Bytes -Offset ([ref]$cursor)
    $imprint = Read-DerElement -Bytes $Bytes -Offset ([ref]$cursor)
    if ($version.Tag -ne 0x02 -or $policy.Tag -ne 0x06 -or $imprint.Tag -ne 0x30) {
        throw "RFC3161 TSTInfo has an invalid version, policy, or messageImprint"
    }

    $imprintCursor = $imprint.ContentOffset
    $algorithm = Read-DerElement -Bytes $Bytes -Offset ([ref]$imprintCursor)
    $hashedMessage = Read-DerElement -Bytes $Bytes -Offset ([ref]$imprintCursor)
    if ($algorithm.Tag -ne 0x30 -or $hashedMessage.Tag -ne 0x04 -or
        $imprintCursor -ne $imprint.End) {
        throw "RFC3161 messageImprint is malformed"
    }
    $algorithmCursor = $algorithm.ContentOffset
    $algorithmOid = Read-DerElement -Bytes $Bytes -Offset ([ref]$algorithmCursor)
    if ($algorithmOid.Tag -ne 0x06) {
        throw "RFC3161 messageImprint has no digest algorithm OID"
    }
    $digestOid = ConvertFrom-DerOid -Bytes $Bytes `
        -Offset $algorithmOid.ContentOffset -Length $algorithmOid.ContentLength
    $digestNames = @{
        "1.3.14.3.2.26" = "SHA1"
        "2.16.840.1.101.3.4.2.1" = "SHA256"
        "2.16.840.1.101.3.4.2.2" = "SHA384"
        "2.16.840.1.101.3.4.2.3" = "SHA512"
    }
    $digestName = if ($digestNames.ContainsKey($digestOid)) {
        $digestNames[$digestOid]
    }
    else {
        "OID:$digestOid"
    }
    $message = [byte[]]::new($hashedMessage.ContentLength)
    [Array]::Copy(
        $Bytes,
        $hashedMessage.ContentOffset,
        $message,
        0,
        $hashedMessage.ContentLength
    )
    return [pscustomobject]@{
        Protocol = "RFC3161"
        ContentTypeOid = $script:TstInfoContentTypeOid
        DigestOid = $digestOid
        DigestAlgorithm = $digestName
        MessageImprintLength = $message.Length
        MessageImprintHex = ([BitConverter]::ToString($message)).Replace("-", "").ToLowerInvariant()
        MessageImprintBytes = $message
    }
}

function Test-ByteArraysEqualFixedTime {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)][byte[]]$Left,
        [Parameter(Mandatory = $true)][byte[]]$Right
    )

    if ($Left.Length -ne $Right.Length) {
        return $false
    }
    $difference = 0
    for ($index = 0; $index -lt $Left.Length; $index++) {
        $difference = $difference -bor ([int]$Left[$index] -bxor [int]$Right[$index])
    }
    return $difference -eq 0
}

function Assert-Rfc3161MessageImprint {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)]
        [object]$TimestampInfo,
        [Parameter(Mandatory = $true)]
        [byte[]]$ParentSignatureBytes,
        [string]$Location = "signed file"
    )

    $algorithm = switch ($TimestampInfo.DigestAlgorithm) {
        "SHA1" { [Security.Cryptography.SHA1]::Create(); break }
        "SHA256" { [Security.Cryptography.SHA256]::Create(); break }
        "SHA384" { [Security.Cryptography.SHA384]::Create(); break }
        "SHA512" { [Security.Cryptography.SHA512]::Create(); break }
        default { throw "$Location uses an unsupported RFC3161 messageImprint digest" }
    }
    try {
        $actual = $algorithm.ComputeHash($ParentSignatureBytes)
    }
    finally {
        $algorithm.Dispose()
    }
    $expected = [byte[]]$TimestampInfo.MessageImprintBytes
    if (-not (Test-ByteArraysEqualFixedTime -Left $actual -Right $expected)) {
        throw "$Location RFC3161 messageImprint does not bind the parent Authenticode signature"
    }
}

function Assert-Rfc3161Sha256TimestampInfo {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)]
        [object]$TimestampInfo,
        [string]$Location = "signed file"
    )

    if ($TimestampInfo.Protocol -ne "RFC3161" -or
        $TimestampInfo.ContentTypeOid -ne $script:TstInfoContentTypeOid -or
        $TimestampInfo.DigestOid -ne $script:Sha256Oid -or
        $TimestampInfo.DigestAlgorithm -ne "SHA256" -or
        $TimestampInfo.MessageImprintLength -ne 32) {
        throw "$Location does not contain an RFC3161 timestamp with a SHA-256 messageImprint"
    }
}

function Resolve-AuthenticodeTimestampProtocol {
    [CmdletBinding()]
    param(
        [string[]]$UnsignedAttributeOids = @(),
        [bool]$HasLegacyCounterSigner = $false
    )

    $hasRfc3161 = @($UnsignedAttributeOids | Where-Object {
        $_ -eq $script:Rfc3161CounterSignatureOid
    }).Count -gt 0
    $hasLegacy = $HasLegacyCounterSigner -or @($UnsignedAttributeOids | Where-Object {
        $_ -eq $script:LegacyCounterSignatureOid
    }).Count -gt 0
    if ($hasRfc3161 -and $hasLegacy) {
        throw "signature contains ambiguous RFC3161 and legacy timestamps"
    }
    if ($hasRfc3161) { return "RFC3161" }
    if ($hasLegacy) { return "AuthenticodeLegacy" }
    return "None"
}

function Get-AuthenticodeTimestampInfo {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)]
        [string]$Path,
        [Parameter(Mandatory = $true)]
        [string]$SignerThumbprint
    )

    Add-Type -AssemblyName System.Security
    $resolved = (Resolve-Path -LiteralPath $Path).Path
    $encoded = [Ojos.AuthenticodeNative]::ReadEmbeddedSignedCms($resolved)
    $signedCms = [Security.Cryptography.Pkcs.SignedCms]::new()
    $signedCms.Decode($encoded)
    $normalizedThumbprint = $SignerThumbprint.Replace(" ", "").ToUpperInvariant()
    $matchingSigners = @($signedCms.SignerInfos | Where-Object {
        $null -ne $_.Certificate -and
        $_.Certificate.Thumbprint.Replace(" ", "").ToUpperInvariant() -eq $normalizedThumbprint
    })
    if ($matchingSigners.Count -ne 1) {
        throw "expected exactly one Authenticode signer matching $SignerThumbprint in $resolved"
    }
    $signer = $matchingSigners[0]
    $unsignedOids = @($signer.UnsignedAttributes | ForEach-Object { $_.Oid.Value })
    $protocol = Resolve-AuthenticodeTimestampProtocol `
        -UnsignedAttributeOids $unsignedOids `
        -HasLegacyCounterSigner ($signer.CounterSignerInfos.Count -gt 0)
    if ($protocol -eq "None") {
        return [pscustomobject]@{
            Protocol = "None"
            ContentTypeOid = $null
            DigestOid = $null
            DigestAlgorithm = "NONE"
            MessageImprintLength = 0
            MessageImprintHex = $null
            MessageImprintBytes = [byte[]]@()
            TokenSignatureValid = $null
            ParentSignatureDigestVerified = $null
            TimestampSubject = $null
            TimestampThumbprint = $null
        }
    }
    if ($protocol -eq "AuthenticodeLegacy") {
        $legacySigners = @($signer.CounterSignerInfos)
        if ($legacySigners.Count -ne 1 -or $null -eq $legacySigners[0].Certificate) {
            throw "the Authenticode signer in $resolved has ambiguous legacy timestamp signers"
        }
        return [pscustomobject]@{
            Protocol = "AuthenticodeLegacy"
            ContentTypeOid = $null
            DigestOid = $null
            DigestAlgorithm = "UNKNOWN"
            MessageImprintLength = 0
            MessageImprintHex = $null
            MessageImprintBytes = [byte[]]@()
            TokenSignatureValid = $null
            ParentSignatureDigestVerified = $null
            TimestampSubject = $legacySigners[0].Certificate.Subject
            TimestampThumbprint = $legacySigners[0].Certificate.Thumbprint
        }
    }

    $attributes = @($signer.UnsignedAttributes | Where-Object {
        $_.Oid.Value -eq $script:Rfc3161CounterSignatureOid
    })
    if ($attributes.Count -ne 1 -or $attributes[0].Values.Count -ne 1) {
        throw "the Authenticode signer in $resolved must contain exactly one RFC3161 token"
    }
    $timestampToken = [Security.Cryptography.Pkcs.SignedCms]::new()
    $timestampToken.Decode($attributes[0].Values[0].RawData)
    if ($timestampToken.ContentInfo.ContentType.Value -ne $script:TstInfoContentTypeOid) {
        throw "the timestamp token in $resolved is not RFC3161 TSTInfo"
    }
    try {
        $timestampToken.CheckSignature($true)
    }
    catch {
        throw "the RFC3161 timestamp token signature in $resolved is invalid: $($_.Exception.Message)"
    }
    $timestampSigners = @($timestampToken.SignerInfos)
    if ($timestampSigners.Count -ne 1 -or $null -eq $timestampSigners[0].Certificate) {
        throw "the RFC3161 timestamp token in $resolved has no unique signing certificate"
    }
    $timestampInfo = Read-Rfc3161TstInfo -Bytes $timestampToken.ContentInfo.Content
    Assert-Rfc3161MessageImprint `
        -TimestampInfo $timestampInfo `
        -ParentSignatureBytes ([byte[]]$signer.GetSignature()) `
        -Location $resolved
    $timestampInfo | Add-Member -NotePropertyName TokenSignatureValid -NotePropertyValue $true
    $timestampInfo | Add-Member `
        -NotePropertyName ParentSignatureDigestVerified `
        -NotePropertyValue $true
    $timestampInfo | Add-Member `
        -NotePropertyName TimestampSubject `
        -NotePropertyValue $timestampSigners[0].Certificate.Subject
    $timestampInfo | Add-Member `
        -NotePropertyName TimestampThumbprint `
        -NotePropertyValue $timestampSigners[0].Certificate.Thumbprint
    return $timestampInfo
}
