param(
  [string]$Version = "v0.1.0",
  [switch]$SkipDockerBuild
)

$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"

$Root = Split-Path -Parent $PSScriptRoot
$OutDir = Join-Path $Root ".tmp/release/$Version"
$BinDir = Join-Path $OutDir "bin"
$PackageDir = Join-Path $OutDir "packages"
$FrontendDir = Join-Path $OutDir "frontend"

function Step($Name, [scriptblock]$Body) {
  Write-Host ""
  Write-Host "==> $Name" -ForegroundColor Cyan
  & $Body
}

function Run {
  param([string]$Command, [Parameter(ValueFromRemainingArguments = $true)][object[]]$CommandArgs)
  if ($CommandArgs.Count -eq 1 -and $CommandArgs[0] -is [array]) { $CommandArgs = $CommandArgs[0] }
  & $Command @CommandArgs
  if ($LASTEXITCODE -ne 0) { throw "$Command failed with exit code $LASTEXITCODE" }
}

function Copy-Artifact($Source, $Destination) {
  if (-not (Test-Path $Source)) { throw "artifact missing: $Source" }
  Copy-Item -Force $Source $Destination
}

function Add-Checksum($Path, [System.Collections.Generic.List[string]]$Rows) {
  $hash = Get-FileHash -Algorithm SHA256 -LiteralPath $Path
  $rel = Resolve-Path -LiteralPath $Path -Relative
  $Rows.Add("$($hash.Hash.ToLowerInvariant())  $rel") | Out-Null
}

function Write-Utf8NoBom($Path, [string]$Content) {
  $encoding = [System.Text.UTF8Encoding]::new($false)
  [System.IO.File]::WriteAllText($Path, $Content, $encoding)
}

function Test-LoopbackProxy([string]$Value) {
  if ([string]::IsNullOrWhiteSpace($Value)) { return $false }
  try {
    $uri = [Uri]$Value
    return @("127.0.0.1", "localhost", "::1") -contains $uri.Host
  } catch {
    return $false
  }
}

function Invoke-WithoutLoopbackProxy([scriptblock]$Body) {
  $names = @("HTTP_PROXY", "HTTPS_PROXY", "http_proxy", "https_proxy")
  $saved = @{}
  foreach ($name in $names) {
    $saved[$name] = [Environment]::GetEnvironmentVariable($name, "Process")
    if (Test-LoopbackProxy $saved[$name]) {
      [Environment]::SetEnvironmentVariable($name, "", "Process")
    }
  }

  try {
    & $Body
  } finally {
    foreach ($name in $names) {
      [Environment]::SetEnvironmentVariable($name, $saved[$name], "Process")
    }
  }
}

Push-Location $Root
try {
  if (Test-Path $OutDir) { Remove-Item -Recurse -Force $OutDir }
  New-Item -ItemType Directory -Force $BinDir, $PackageDir, $FrontendDir | Out-Null

  Step "Rust release binaries" {
    Run "cargo" @("build", "--release", "-p", "ojosctl", "-p", "ojos-installer-tui")
    Copy-Artifact "target/release/ojosctl.exe" (Join-Path $BinDir "ojosctl.exe")
    Copy-Artifact "target/release/ojos-installer-tui.exe" (Join-Path $BinDir "ojos-installer-tui.exe")
  }

  Step "Sample module package" {
    Run "cargo" @("run", "-q", "-p", "ojosctl", "--", "--json", "module", "package", "modules/sample-hello", "-o", (Join-Path $PackageDir "sample-hello.ojosmod"))
    Run "cargo" @("run", "-q", "-p", "ojosctl", "--", "--json", "module", "verify", (Join-Path $PackageDir "sample-hello.ojosmod"))
  }

  Step "Frontend build" {
    Push-Location "frontend"
    try {
      Run "npm" @("run", "build")
    } finally {
      Pop-Location
    }
    Copy-Item -Recurse -Force "frontend/dist" (Join-Path $FrontendDir "dist")
  }

  if (-not $SkipDockerBuild) {
    Step "Docker images" {
      Invoke-WithoutLoopbackProxy {
        Run "docker" @("compose", "--env-file", ".env.example", "-f", "deploy/compose/docker-compose.yml", "build", "gateway", "auth", "problem-api", "judge-api", "judge-worker", "module-installer")
      }
    }
  }

  Step "Release documents" {
    foreach ($doc in @(
      "docs/release/v0.1.0-release-notes.md",
      "docs/release/v0.1.0-ship-checklist.md",
      "docs/release/v0.1.0-acceptance-report.md",
      "docs/release/v0.1.0-known-limitations.md",
      "docs/release/v0.1.0-artifacts.md"
    )) {
      if (Test-Path $doc) {
        Copy-Item -Force $doc $OutDir
      }
    }
  }

  $rows = [System.Collections.Generic.List[string]]::new()
  Get-ChildItem -Path $OutDir -Recurse -File |
    Where-Object { $_.Name -ne "checksums.sha256" -and $_.Name -ne "artifact-manifest.json" } |
    Sort-Object FullName |
    ForEach-Object { Add-Checksum $_.FullName $rows }
  [System.IO.File]::WriteAllLines((Join-Path $OutDir "checksums.sha256"), [string[]]$rows, [System.Text.UTF8Encoding]::new($false))

  $manifest = [ordered]@{
    version = $Version
    created_at = (Get-Date).ToUniversalTime().ToString("o")
    output_dir = ".tmp/release/$Version"
    artifacts = @(
      "bin/ojosctl.exe",
      "bin/ojos-installer-tui.exe",
      "packages/sample-hello.ojosmod",
      "frontend/dist",
      "checksums.sha256"
    )
    docker_images = if ($SkipDockerBuild) { "skipped" } else { "built_by_docker_compose" }
  }
  Write-Utf8NoBom (Join-Path $OutDir "artifact-manifest.json") ($manifest | ConvertTo-Json -Depth 5)
  $manifest | ConvertTo-Json -Depth 5
} finally {
  Pop-Location
}
