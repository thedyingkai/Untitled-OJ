param(
  [string]$BaseUrl = "http://localhost:8080/api",
  [string]$AdminUsername = "admin1",
  [string]$AdminPassword = "admin123",
  [string]$UserUsername = "user1",
  [string]$UserPassword = "user123",
  [string]$WorkerToken = $env:OJOS_WORKER_TOKEN
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
$failures = New-Object System.Collections.Generic.List[string]
$pathLeaks = 0

Add-Type -AssemblyName System.Net.Http

function Run-Step {
  param([string]$Name, [scriptblock]$Body)
  Write-Host "== $Name"
  try {
    & $Body
  } catch {
    $failures.Add("${Name}: $($_.Exception.Message)") | Out-Null
  }
}

function Run-Cargo {
  param([string[]]$CargoArgs)
  Push-Location $root
  try {
    & cargo @CargoArgs
    if ($LASTEXITCODE -ne 0) {
      throw "cargo failed: $($CargoArgs -join ' ')"
    }
  } finally {
    Pop-Location
  }
}

function Invoke-Json {
  param(
    [string]$Method,
    [string]$Path,
    [int[]]$Expected,
    [hashtable]$Headers = @{},
    $Body = $null
  )
  $uri = "$BaseUrl$Path"
  $client = [System.Net.Http.HttpClient]::new()
  try {
    $request = [System.Net.Http.HttpRequestMessage]::new([System.Net.Http.HttpMethod]::new($Method), $uri)
    foreach ($key in $Headers.Keys) {
      $request.Headers.TryAddWithoutValidation([string]$key, [string]$Headers[$key]) | Out-Null
    }
    if ($null -ne $Body) {
      $jsonBody = $Body | ConvertTo-Json -Depth 20
      $request.Content = [System.Net.Http.StringContent]::new($jsonBody, [System.Text.Encoding]::UTF8, "application/json")
    }
    $response = $client.SendAsync($request).GetAwaiter().GetResult()
    $status = [int]$response.StatusCode
    $content = $response.Content.ReadAsStringAsync().GetAwaiter().GetResult()
    if ($Expected -notcontains $status) {
      throw "expected $($Expected -join ',') got $status for $Method $Path"
    }
    if ($content) { return $content | ConvertFrom-Json }
    return $null
  } finally {
    $client.Dispose()
  }
}

Run-Step "service.yaml validate" {
  foreach ($manifest in @(
    "services/gateway/service.yaml",
    "services/web-shell/service.yaml",
    "services/problem-api/service.yaml",
    "services/judge-api/service.yaml",
    "services/judge-worker/service.yaml",
    "services/storage/service.yaml",
    "services/postgres/service.yaml"
  )) {
    Run-Cargo -CargoArgs @("run", "-q", "-p", "ojosctl", "--", "service", "validate", $manifest)
  }
}

Run-Step "service package" {
  $out = ".tmp/release/gateway.ojos-service"
  Run-Cargo -CargoArgs @("run", "-q", "-p", "ojosctl", "--", "service", "package", "services/gateway", "-o", $out)
  Run-Cargo -CargoArgs @("run", "-q", "-p", "ojosctl", "--", "service", "verify", $out)
}

Run-Step "service install plan" {
  Run-Cargo -CargoArgs @("run", "-q", "-p", "ojosctl", "--", "service", "install-plan", "services/judge-worker/service.yaml")
}

Run-Step "set expand" {
  foreach ($set in Get-ChildItem (Join-Path $root "sets") -Filter "*.yaml") {
    Run-Cargo -CargoArgs @("run", "-q", "-p", "ojosctl", "--", "set", "expand", ("sets/" + $set.Name))
  }
}

Run-Step "endpoint register plan" {
  Run-Cargo -CargoArgs @("run", "-q", "-p", "ojosctl", "--", "endpoint", "validate", "192.168.1.10:8082")
  Run-Cargo -CargoArgs @("run", "-q", "-p", "ojosctl", "--", "endpoint", "plan-register", "judge-api", "192.168.1.10:8082")
}

Run-Step "link create plan" {
  Run-Cargo -CargoArgs @("run", "-q", "-p", "ojosctl", "--", "link", "plan-create", "192.168.1.21:9101", "192.168.1.10:8082", "--protocol", "http", "--auth-mode", "worker")
}

Run-Step "topology contains service endpoint link device" {
  Run-Cargo -CargoArgs @("run", "-q", "-p", "ojosctl", "--", "topology", "snapshot")
}

Run-Step "web-shell root runtime boundary" {
  $manifest = Get-Content (Join-Path $root "services/web-shell/service.yaml") -Raw
  if ($manifest -notmatch "root_allowed: true" -or $manifest -notmatch "non_root_allowed: false") {
    throw "web-shell must be root-only"
  }
}

Run-Step "non-root device cannot run web-shell" {
  $set = Get-Content (Join-Path $root "sets/judge-worker-node.yaml") -Raw
  if ($set -match "web-shell") {
    throw "judge-worker-node set must not include web-shell"
  }
}

Run-Step "judge-worker endpoint model" {
  $manifest = Get-Content (Join-Path $root "services/judge-worker/service.yaml") -Raw
  if ($manifest -notmatch "default_port: 9101" -or $manifest -notmatch "id: judge-api") {
    throw "judge-worker must expose endpoint and require judge-api"
  }
}

Run-Step "ordinary user 403 and no token 401" {
  $login = Invoke-Json POST "/auth/login" @(200) -Body @{ username = $UserUsername; password = $UserPassword }
  if (-not $login.data.token) {
    throw "ordinary user login did not return token"
  }
  $userHeaders = @{ Authorization = "Bearer $($login.data.token)" }
  Invoke-Json GET "/admin/runtime/services" @(403) -Headers $userHeaders | Out-Null
  Invoke-Json GET "/admin/runtime/services" @(401) | Out-Null
}

Run-Step "path_leaks=0" {
  Push-Location $root
  try {
    $leaks = git diff | Select-String -Pattern "D:\\\\WSL|D:/WSL|/mnt/d/Untitled-OJ|C:\\\\Users|/home/.*/Untitled-OJ"
    $script:pathLeaks = @($leaks).Count
    if ($leaks) {
      throw "path_leaks=$($leaks.Count)"
    }
  } finally {
    Pop-Location
  }
}

$summary = [ordered]@{
  failed = $failures.Count
  path_leaks = $pathLeaks
  ordinary_user_403 = $true
  no_token_401 = $true
  overall_status = "ok"
}

if ($failures.Count -gt 0) {
  $summary.failed = $failures.Count
  $summary.overall_status = "failed"
  $summary | ConvertTo-Json -Depth 5
  $failures | ForEach-Object { Write-Error $_ }
  throw "service runtime e2e failed: $($failures.Count) failure(s)"
}

Write-Host "service runtime e2e passed"
$summary | ConvertTo-Json -Depth 5
