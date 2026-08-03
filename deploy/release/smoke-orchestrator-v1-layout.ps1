[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$ArtifactDirectory,
    [Parameter(Mandatory = $true)]
    [string]$Version,
    [Parameter(Mandatory = $true)]
    [ValidateSet("compat", "ga")]
    [string]$Channel
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"
$Version = $Version.TrimStart([char]"v")
$ArtifactDirectory = (Resolve-Path -LiteralPath $ArtifactDirectory).Path
$base = "ojos-orchestrator-$Version-$Channel-windows-x64"
$archive = Join-Path $ArtifactDirectory "$base.zip"
$msi = Join-Path $ArtifactDirectory "ojos-orchestrator-$Version-windows-x64.msi"
if (-not (Test-Path -LiteralPath $archive -PathType Leaf) -or
    -not (Test-Path -LiteralPath $msi -PathType Leaf)) {
    throw "portable ZIP and MSI are required in $ArtifactDirectory"
}

$tempRoot = [IO.Path]::GetFullPath([IO.Path]::GetTempPath())
$work = Join-Path $tempRoot ("ojos-orchestrator-layout-" + [Guid]::NewGuid().ToString("N"))
[void](New-Item -ItemType Directory -Path $work)

function Get-ExactDesktop([string]$Root) {
    $matches = @(Get-ChildItem -LiteralPath $Root -Recurse -File |
        Where-Object { $_.Name -eq "ojos-orchestrator-desktop.exe" })
    if ($matches.Count -ne 1) {
        throw "expected exactly one Desktop executable under $Root, found $($matches.Count)"
    }
    return $matches[0].FullName
}

function Invoke-Process(
    [string]$FilePath,
    [string[]]$Arguments,
    [string]$WorkingDirectory,
    [int]$TimeoutMilliseconds,
    [hashtable]$Environment
) {
    $startInfo = [Diagnostics.ProcessStartInfo]::new()
    $startInfo.FileName = $FilePath
    $startInfo.WorkingDirectory = $WorkingDirectory
    $startInfo.UseShellExecute = $false
    $startInfo.CreateNoWindow = $true
    foreach ($argument in $Arguments) {
        [void]$startInfo.ArgumentList.Add($argument)
    }
    foreach ($entry in $Environment.GetEnumerator()) {
        $startInfo.Environment[$entry.Key] = [string]$entry.Value
    }
    $process = [Diagnostics.Process]::new()
    $process.StartInfo = $startInfo
    if (-not $process.Start()) {
        throw "failed to start $FilePath"
    }
    if (-not $process.WaitForExit($TimeoutMilliseconds)) {
        $process.Kill($true)
        $process.WaitForExit()
        throw "$FilePath did not exit within $TimeoutMilliseconds ms"
    }
    return $process.ExitCode
}

function Invoke-DesktopSmoke([string]$Label, [string]$Executable) {
    $dataDirectory = Join-Path $work "data-$Label"
    [void](New-Item -ItemType Directory -Path $dataDirectory)
    Write-Host "smoke-orchestrator-v1-layout: starting $Label layout"
    $desktopProcess = @{
        FilePath = $Executable
        Arguments = @("--data-dir", $dataDirectory)
        WorkingDirectory = (Join-Path $work "cwd")
        TimeoutMilliseconds = 120000
        Environment = @{ OJOS_DESKTOP_SMOKE = "1" }
    }
    $exitCode = Invoke-Process @desktopProcess
    if ($exitCode -ne 0) {
        throw "$Label Desktop smoke exited with code $exitCode"
    }
}

try {
    $portable = Join-Path $work "portable"
    $installed = Join-Path $work "msi"
    [void](New-Item -ItemType Directory -Path $portable)
    [void](New-Item -ItemType Directory -Path $installed)
    [void](New-Item -ItemType Directory -Path (Join-Path $work "cwd"))

    Expand-Archive -LiteralPath $archive -DestinationPath $portable
    $portableDesktop = Get-ExactDesktop $portable
    if ((Split-Path -Leaf (Split-Path -Parent $portableDesktop)) -ne "bin") {
        throw "portable Desktop must be stored below the archive bin directory"
    }
    Invoke-DesktopSmoke -Label "portable" -Executable $portableDesktop

    $msiProcess = @{
        FilePath = "msiexec.exe"
        Arguments = @("/a", $msi, "/qn", "/norestart", "TARGETDIR=$installed")
        WorkingDirectory = $work
        TimeoutMilliseconds = 180000
        Environment = @{}
    }
    $msiExitCode = Invoke-Process @msiProcess
    if ($msiExitCode -notin @(0, 3010)) {
        throw "MSI administrative extraction exited with code $msiExitCode"
    }
    $installedDesktop = Get-ExactDesktop $installed
    Invoke-DesktopSmoke -Label "msi" -Executable $installedDesktop

    Write-Host "Desktop portable ZIP and MSI resource layouts started successfully"
}
finally {
    $resolvedWork = [IO.Path]::GetFullPath($work)
    if ($resolvedWork.StartsWith($tempRoot, [StringComparison]::OrdinalIgnoreCase)) {
        Remove-Item -LiteralPath $resolvedWork -Recurse -Force -ErrorAction SilentlyContinue
    }
}
