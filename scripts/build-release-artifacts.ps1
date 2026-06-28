param(
  [string]$Version = "service-first",
  [switch]$SkipDockerBuild
)

$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"

$Root = Split-Path -Parent $PSScriptRoot
$OutDir = Join-Path $Root ".tmp/release/$Version"
$BinDir = Join-Path $OutDir "bin"
$PackageDir = Join-Path $OutDir "packages"

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

Push-Location $Root
try {
  if (Test-Path $OutDir) { Remove-Item -Recurse -Force $OutDir }
  New-Item -ItemType Directory -Force $BinDir, $PackageDir | Out-Null

  Step "Rust release binaries" {
    Run "cargo" @("build", "--release", "-p", "ojosctl", "-p", "ojos-installer-tui")
    Copy-Artifact "target/release/ojosctl.exe" (Join-Path $BinDir "ojosctl.exe")
    Copy-Artifact "target/release/ojos-installer-tui.exe" (Join-Path $BinDir "ojos-installer-tui.exe")
  }

  Step "Service packages" {
    foreach ($service in @("gateway", "problem-api", "judge-api", "judge-worker")) {
      $out = Join-Path $PackageDir "$service.ojossvc"
      Run "cargo" @("run", "-q", "-p", "ojosctl", "--", "--json", "service", "package", "services/$service", "-o", $out)
      Run "cargo" @("run", "-q", "-p", "ojosctl", "--", "--json", "service", "verify", $out)
    }
  }

  if (-not $SkipDockerBuild) {
    Step "Docker images" {
      Run "docker" @("compose", "--env-file", ".env.example", "-f", "deploy/compose/docker-compose.yml", "build", "gateway", "auth", "problem-api", "judge-api", "judge-worker", "root-runtime-manager")
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
      "packages/*.ojossvc",
      "checksums.sha256"
    )
    docker_images = if ($SkipDockerBuild) { "skipped" } else { "built_by_docker_compose" }
  }
  [System.IO.File]::WriteAllText((Join-Path $OutDir "artifact-manifest.json"), ($manifest | ConvertTo-Json -Depth 5), [System.Text.UTF8Encoding]::new($false))
  $manifest | ConvertTo-Json -Depth 5
} finally {
  Pop-Location
}
