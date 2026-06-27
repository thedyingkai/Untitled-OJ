param(
  [switch]$SkipDockerBuild,
  [switch]$SkipControlledApply,
  [switch]$SkipFrontend,
  [switch]$SkipRust,
  [switch]$SkipGo,
  [switch]$RunControlledApply,
  [string]$BaseUrl = "http://localhost:8080/api",
  [string]$AdminUsername = "admin1",
  [string]$AdminPassword = "admin123",
  [string]$UserUsername = "user1",
  [string]$UserPassword = "user123",
  [string]$WorkerToken = $env:OJOS_WORKER_TOKEN
)

$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"

$Root = Split-Path -Parent $PSScriptRoot
$ReportDir = Join-Path $Root ".tmp/agent/reports/kernel-acceptance"
New-Item -ItemType Directory -Force $ReportDir | Out-Null

function Run-Step {
  param([string]$Name, [scriptblock]$Body)
  Write-Host ""
  Write-Host "==> $Name" -ForegroundColor Cyan
  & $Body
}

function Run-Capture {
  param([string]$Name, [string]$Command, [string[]]$CommandArgs)
  $previous = $ErrorActionPreference
  $ErrorActionPreference = "Continue"
  try {
    $output = & $Command @CommandArgs 2>&1
    $code = $LASTEXITCODE
  } finally {
    $ErrorActionPreference = $previous
  }
  $text = ($output | Out-String)
  [System.IO.File]::WriteAllText((Join-Path $ReportDir "$Name.log"), $text, [System.Text.UTF8Encoding]::new($false))
  return [pscustomobject]@{ ExitCode = $code; Text = $text }
}

function Last-JsonObject {
  param([string]$Text)
  $lines = @($Text -split "`r?`n")
  for ($start = 0; $start -lt $lines.Count; $start++) {
    $candidate = (($lines[$start..($lines.Count - 1)]) -join "`n").Trim()
    if (-not $candidate.StartsWith("{")) { continue }
    try { return ($candidate | ConvertFrom-Json) } catch {}
  }
  return $null
}

if (-not $WorkerToken) {
  $workerTokenPattern = "^" + "OJOS_WORKER_TOKEN" + "="
  $line = Select-String -Path (Join-Path $Root ".env") -Pattern $workerTokenPattern -ErrorAction SilentlyContinue | Select-Object -First 1
  if ($line) {
    $WorkerToken = $line.Line.Split("=", 2)[1]
  }
}

$summary = [ordered]@{
  static_failed = $false
  api_failed = $false
  compat_failed = $false
  path_leaks = 0
  admin_health_status = ""
  admin_health_judge_status = ""
  module_compat = "not_run"
  controlled_apply = "skipped"
  overall_status = "unknown"
}

Push-Location $Root
try {
  Run-Step "static verification" {
    $args = @("-NoProfile", "-File", "scripts\verify-static.ps1")
    if ($SkipDockerBuild) { $args += "-SkipDockerBuild" }
    if ($SkipFrontend) { $args += "-SkipFrontend" }
    if ($SkipRust) { $args += "-SkipRust" }
    if ($SkipGo) { $args += "-SkipGo" }
    $result = Run-Capture "verify-static" "powershell" $args
    if ($result.ExitCode -ne 0) {
      $summary.static_failed = $true
    }
  }

  Run-Step "api e2e" {
    if (-not $WorkerToken) { throw "WorkerToken is required for API e2e" }
    $args = @(
      "-NoProfile", "-File", "scripts\e2e-api.ps1",
      "-BaseUrl", $BaseUrl,
      "-AdminUsername", $AdminUsername,
      "-AdminPassword", $AdminPassword,
      "-UserUsername", $UserUsername,
      "-UserPassword", $UserPassword,
      "-WorkerToken", $WorkerToken
    )
    $result = Run-Capture "e2e-api" "powershell" $args
    $json = Last-JsonObject $result.Text
    if ($result.ExitCode -ne 0 -or $null -eq $json) {
      $summary.api_failed = $true
    } else {
      if ([int]$json.failed -ne 0) { $summary.api_failed = $true }
      $summary.path_leaks += [int]$json.path_leaks
      $summary.admin_health_status = [string]$json.admin_health_status
      $summary.admin_health_judge_status = [string]$json.admin_health_judge_status
    }
  }

  Run-Step "module compatibility" {
    $args = @(
      "-NoProfile", "-File", "scripts\e2e-module-compat.ps1",
      "-BaseUrl", $BaseUrl,
      "-AdminUsername", $AdminUsername,
      "-AdminPassword", $AdminPassword,
      "-UserUsername", $UserUsername,
      "-UserPassword", $UserPassword
    )
    $result = Run-Capture "e2e-module-compat" "powershell" $args
    $json = Last-JsonObject $result.Text
    if ($result.ExitCode -ne 0 -or $null -eq $json) {
      $summary.compat_failed = $true
      $summary.module_compat = "failed"
    } else {
      if ([int]$json.failed -ne 0) { $summary.compat_failed = $true }
      $summary.path_leaks += [int]$json.path_leaks
      $summary.module_compat = [string]$json.sample_module_compat
    }
  }

  Run-Step "ojosctl smoke" {
    $commands = @(
      @("run", "-q", "-p", "ojosctl", "--", "module", "doctor"),
      @("run", "-q", "-p", "ojosctl", "--", "module", "validate", "modules/sample-hello/module.yaml", "--repo-root", "."),
      @("run", "-q", "-p", "ojosctl", "--", "runtime", "services"),
      @("run", "-q", "-p", "ojosctl", "--", "runtime", "plan-restart", "problem-api", "--out", ".tmp/agent/scratch/acceptance-problem-api-restart.json"),
      @("run", "-q", "-p", "ojosctl", "--", "runtime", "apply-plan", ".tmp/agent/scratch/acceptance-problem-api-restart.json", "--dry-run")
    )
    $index = 0
    foreach ($cmdArgs in $commands) {
      $index += 1
      $result = Run-Capture "ojosctl-smoke-$index" "cargo" ([string[]]$cmdArgs)
      if ($result.ExitCode -ne 0) {
        throw "ojosctl smoke failed at step $index"
      }
    }
  }

  if ($RunControlledApply -and -not $SkipControlledApply) {
    Run-Step "controlled apply smoke" {
      $planPath = ".tmp/agent/scratch/acceptance-problem-api-restart.json"
      $result = Run-Capture "controlled-apply" "cargo" ([string[]]@("run", "-q", "-p", "ojosctl", "--", "runtime", "apply-plan", $planPath, "--confirm"))
      if ($result.ExitCode -ne 0) {
        $summary.controlled_apply = "failed"
        throw "controlled apply smoke failed"
      }
      $summary.controlled_apply = "succeeded"
    }
  } else {
    $summary.controlled_apply = "skipped"
  }

  if (-not $summary.static_failed -and -not $summary.api_failed -and -not $summary.compat_failed -and [int]$summary.path_leaks -eq 0) {
    $summary.overall_status = "ok"
  } else {
    $summary.overall_status = "failed"
  }
} finally {
  Pop-Location
  $summary | ConvertTo-Json -Depth 5 | Set-Content (Join-Path $ReportDir "summary.json")
  $summary | ConvertTo-Json -Depth 5
}

if ($summary.overall_status -ne "ok") {
  exit 1
}
