<#
Module SDK compatibility harness.

Runs against a live Docker control plane through Gateway. Temporary scaffold and
packages are written only under .tmp/agent/scratch and must not be committed.
#>

param(
  [string]$BaseUrl = "http://localhost:8080/api",
  [string]$AdminUsername = "admin1",
  [string]$AdminPassword = "admin123",
  [string]$UserUsername = "user1",
  [string]$UserPassword = "user123"
)

$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"

$Root = (Resolve-Path ".").Path
$ScratchRel = ".tmp/agent/scratch/module-compat"
$ReportRel = ".tmp/agent/reports/module-compat"
$Scratch = Join-Path $Root $ScratchRel
$ReportDir = Join-Path $Root $ReportRel
New-Item -ItemType Directory -Force $Scratch, $ReportDir | Out-Null

$results = New-Object System.Collections.Generic.List[object]
$failures = New-Object System.Collections.Generic.List[string]
$leaks = New-Object System.Collections.Generic.List[string]
$script:AdminToken = $null
$script:UserToken = $null

$forbiddenTokens = @("D:\", "/mnt/d/", "C:\Users", "/home/", ".env", "tokens.local.json", "target/debug", "frontend/dist")

Add-Type -AssemblyName System.Net.Http
$script:HttpHandler = [System.Net.Http.HttpClientHandler]::new()
$script:HttpHandler.UseProxy = $false
$script:HttpClient = [System.Net.Http.HttpClient]::new($script:HttpHandler)
$script:HttpClient.Timeout = [TimeSpan]::FromSeconds(30)

function ConvertTo-JsonBody($obj) {
  if ($null -eq $obj) { return $null }
  return ($obj | ConvertTo-Json -Depth 30 -Compress)
}

function Scan-Leak([string]$Name, [string]$Text) {
  if ([string]::IsNullOrEmpty($Text)) { return }
  foreach ($token in $forbiddenTokens) {
    if ($Text.Contains($token)) {
      $leaks.Add("$Name contains $token") | Out-Null
    }
  }
}

function Invoke-Api {
  param(
    [string]$Name,
    [string]$Method,
    [string]$Path,
    [object]$Body = $null,
    [string]$Token = $null,
    [int[]]$Expected = @(200)
  )

  $uri = if ($Path.StartsWith("http")) { $Path } else { "$BaseUrl$Path" }
  $status = 0
  $text = ""
  $json = $null
  $request = $null
  $resp = $null
  try {
    $request = [System.Net.Http.HttpRequestMessage]::new([System.Net.Http.HttpMethod]::new($Method.ToUpperInvariant()), $uri)
    if ($Token) {
      $request.Headers.Authorization = [System.Net.Http.Headers.AuthenticationHeaderValue]::new("Bearer", $Token)
    }
    if ($null -ne $Body) {
      $request.Content = [System.Net.Http.StringContent]::new((ConvertTo-JsonBody $Body), [System.Text.Encoding]::UTF8, "application/json")
    }
    $resp = $script:HttpClient.SendAsync($request).GetAwaiter().GetResult()
    $status = [int]$resp.StatusCode
    $text = $resp.Content.ReadAsStringAsync().GetAwaiter().GetResult()
    if ($text.Trim().Length -gt 0) {
      try { $json = $text | ConvertFrom-Json } catch {}
    }
  } catch {
    $text = $_.Exception.Message
    $status = 0
  } finally {
    if ($request) { $request.Dispose() }
    if ($resp) { $resp.Dispose() }
  }

  $ok = $Expected -contains $status
  $summary = $text
  if ($summary.Length -gt 500) { $summary = $summary.Substring(0, 500) + "..." }
  $results.Add([pscustomobject]@{
    name = $Name
    method = $Method
    path = $Path
    status = $status
    expected = ($Expected -join ",")
    ok = $ok
    summary = $summary
  }) | Out-Null
  if (-not $ok) {
    $failures.Add("$Name expected $($Expected -join '/') got $status $summary") | Out-Null
  }
  Scan-Leak $Name $text
  return [pscustomobject]@{ Status = $status; Text = $text; Json = $json; Ok = $ok }
}

function Run-Cmd {
  param([string]$Name, [string]$Exe, [string[]]$CommandArgs)
  $previousErrorActionPreference = $ErrorActionPreference
  $ErrorActionPreference = "Continue"
  try {
    $output = & $Exe @CommandArgs 2>&1
    $code = $LASTEXITCODE
  } finally {
    $ErrorActionPreference = $previousErrorActionPreference
  }
  $text = ($output | Out-String)
  $ok = $code -eq 0
  $results.Add([pscustomobject]@{
    name = $Name
    method = "CMD"
    path = "$Exe $($CommandArgs -join ' ')"
    status = $code
    expected = "0"
    ok = $ok
    summary = $text.Trim()
  }) | Out-Null
  if (-not $ok) {
    $failures.Add("$Name failed with exit code $code $text") | Out-Null
  }
  Scan-Leak $Name $text
  return [pscustomobject]@{ Ok = $ok; Text = $text; Code = $code }
}

function Get-JsonArray($Value, [string]$Property) {
  if ($null -eq $Value) { return @() }
  $prop = $Value.PSObject.Properties[$Property]
  if ($null -eq $prop) { return @() }
  return @($prop.Value)
}

function Has-JsonItem($Items, [string]$Property, [string]$Expected) {
  foreach ($item in @($Items)) {
    $prop = $item.PSObject.Properties[$Property]
    if ($null -ne $prop -and [string]$prop.Value -eq $Expected) { return $true }
  }
  return $false
}

function Get-JsonItem($Items, [string]$Property, [string]$Expected) {
  foreach ($item in @($Items)) {
    $prop = $item.PSObject.Properties[$Property]
    if ($null -ne $prop -and [string]$prop.Value -eq $Expected) { return $item }
  }
  return $null
}

try {
  $scaffoldDir = Join-Path $Scratch "generated-sample-hello"
  $scaffoldDirRel = "$ScratchRel/generated-sample-hello"
  if (Test-Path $scaffoldDir) { Remove-Item -Recurse -Force $scaffoldDir }
  $packagePath = Join-Path $Scratch "sample-hello.ojosmod"
  $packagePathRel = "$ScratchRel/sample-hello.ojosmod"
  if (Test-Path $packagePath) { Remove-Item -Force $packagePath }

  Run-Cmd "ojosctl.module.init.generated" "cargo" @("run", "-q", "-p", "ojosctl", "--", "module", "init", "ojos.sample-generated", "--name", "Sample Generated", "--kind", "feature", "--out", $scaffoldDirRel, "--with-topology") | Out-Null
  if (-not (Test-Path (Join-Path $scaffoldDir "module.yaml"))) {
    $failures.Add("generated scaffold missing module.yaml") | Out-Null
  }
  if (-not (Test-Path (Join-Path $scaffoldDir "frontend/contributions.yaml"))) {
    $failures.Add("generated scaffold missing frontend contributions") | Out-Null
  }
  Run-Cmd "ojosctl.module.validate.sample" "cargo" @("run", "-q", "-p", "ojosctl", "--", "module", "validate", "modules/sample-hello/module.yaml") | Out-Null
  Run-Cmd "ojosctl.module.package.sample" "cargo" @("run", "-q", "-p", "ojosctl", "--", "module", "package", "modules/sample-hello", "-o", $packagePathRel) | Out-Null
  Run-Cmd "ojosctl.module.verify.sample" "cargo" @("run", "-q", "-p", "ojosctl", "--", "module", "verify", $packagePathRel) | Out-Null

  $adminLogin = Invoke-Api "auth.login.admin" POST "/auth/login" @{ username = $AdminUsername; password = $AdminPassword } -Expected @(200)
  $userLogin = Invoke-Api "auth.login.user" POST "/auth/login" @{ username = $UserUsername; password = $UserPassword } -Expected @(200)
  if ($adminLogin.Json.code -eq 0) { $script:AdminToken = $adminLogin.Json.data.token }
  if ($userLogin.Json.code -eq 0) { $script:UserToken = $userLogin.Json.data.token }

  $sampleManifest = @{ manifest_path = "modules/sample-hello/module.yaml"; dry_run = $true }
  Invoke-Api "installer.validate.sample" POST "/admin/modules/validate" $sampleManifest -Token $script:AdminToken -Expected @(200) | Out-Null
  Invoke-Api "installer.validate.sample.user" POST "/admin/modules/validate" $sampleManifest -Token $script:UserToken -Expected @(403) | Out-Null
  Invoke-Api "installer.validate.sample.none" POST "/admin/modules/validate" $sampleManifest -Expected @(401) | Out-Null
  Invoke-Api "installer.install.dry-run.sample" POST "/admin/modules/install" $sampleManifest -Token $script:AdminToken -Expected @(200) | Out-Null
  Invoke-Api "installer.install.apply.sample" POST "/admin/modules/install" @{ manifest_path = "modules/sample-hello/module.yaml"; dry_run = $false } -Token $script:AdminToken -Expected @(200) | Out-Null
  Invoke-Api "installer.install.apply.sample.idempotent" POST "/admin/modules/install" @{ manifest_path = "modules/sample-hello/module.yaml"; dry_run = $false } -Token $script:AdminToken -Expected @(200) | Out-Null
  Invoke-Api "installer.enable.sample" POST "/admin/modules/ojos.sample-hello/enable" @{} -Token $script:AdminToken -Expected @(200) | Out-Null

  $modulesList = Invoke-Api "modules.list.sample" GET "/admin/modules" -Token $script:AdminToken -Expected @(200)
  $detail = Invoke-Api "modules.detail.sample" GET "/admin/modules/ojos.sample-hello" -Token $script:AdminToken -Expected @(200)
  $activeSnapshot = Invoke-Api "runtime-snapshot.sample.enabled" GET "/admin/modules/runtime-snapshot" -Token $script:AdminToken -Expected @(200)
  $topology = Invoke-Api "topology.sample.enabled" GET "/admin/modules/topology" -Token $script:AdminToken -Expected @(200)
  $routesAll = Invoke-Api "runtime.routes.sample.include-disabled" GET "/admin/modules/runtime/routes?include_disabled=true" -Token $script:AdminToken -Expected @(200)
  $services = Invoke-Api "runtime.services.sample" GET "/admin/runtime/services" -Token $script:AdminToken -Expected @(200)
  $serviceDetail = Invoke-Api "runtime.service.sample" GET "/admin/runtime/services/sample-hello-metadata-service" -Token $script:AdminToken -Expected @(200)
  $blockedPlan = Invoke-Api "runtime.service.sample.plan-start.blocked" POST "/admin/runtime/services/sample-hello-metadata-service/plan-start" @{} -Token $script:AdminToken -Expected @(200)
  Invoke-Api "runtime.services.sample.user" GET "/admin/runtime/services" -Token $script:UserToken -Expected @(403) | Out-Null
  Invoke-Api "runtime.services.sample.none" GET "/admin/runtime/services" -Expected @(401) | Out-Null

  if ($modulesList.Json) {
    $items = Get-JsonArray $modulesList.Json "modules"
    if (-not (Has-JsonItem $items "module_id" "ojos.sample-hello")) {
      $failures.Add("sample module missing from module center") | Out-Null
    }
  }
  if ($detail.Json -and [string]$detail.Json.module.module_id -ne "ojos.sample-hello") {
    $failures.Add("sample module detail returned wrong module") | Out-Null
  }
  if ($activeSnapshot.Json) {
    $modules = Get-JsonArray $activeSnapshot.Json "modules"
    $permissions = Get-JsonArray $activeSnapshot.Json "permissions"
    $menus = Get-JsonArray $activeSnapshot.Json "menus"
    $frontendRoutes = Get-JsonArray $activeSnapshot.Json "frontend_routes"
    $servicesInSnapshot = Get-JsonArray $activeSnapshot.Json "services"
    $workersInSnapshot = Get-JsonArray $activeSnapshot.Json "workers"
    $nodes = Get-JsonArray $activeSnapshot.Json.topology "nodes"
    if (-not (Has-JsonItem $modules "module_id" "ojos.sample-hello")) { $failures.Add("active snapshot missing sample module") | Out-Null }
    if (-not (Has-JsonItem $permissions "permission_key" "sample.hello.view")) { $failures.Add("active snapshot missing sample permission") | Out-Null }
    if (-not (Has-JsonItem $menus "menu_key" "sample-hello")) { $failures.Add("active snapshot missing sample menu metadata") | Out-Null }
    if (-not (Has-JsonItem $frontendRoutes "route_path" "/admin/modules/sample-hello")) { $failures.Add("active snapshot missing sample frontend route") | Out-Null }
    if (-not (Has-JsonItem $servicesInSnapshot "service_id" "sample-hello-metadata-service")) { $failures.Add("active snapshot missing sample metadata service") | Out-Null }
    if (-not (Has-JsonItem $workersInSnapshot "service_id" "sample-hello-metadata-worker")) { $failures.Add("active snapshot missing sample metadata worker") | Out-Null }
    if (-not (Has-JsonItem $nodes "module_id" "ojos.sample-hello")) { $failures.Add("active topology nodes missing sample contribution") | Out-Null }
  }
  if ($topology.Json) {
    $nodes = Get-JsonArray $topology.Json "nodes"
    if (-not (Has-JsonItem $nodes "module_id" "ojos.sample-hello")) {
      $failures.Add("topology API missing sample node") | Out-Null
    }
  }
  if ($routesAll.Json) {
    $routes = Get-JsonArray $routesAll.Json "routes"
    $route = Get-JsonItem $routes "prefix" "/api/sample-hello"
    if ($null -eq $route) {
      $failures.Add("include-disabled route table missing sample disabled route") | Out-Null
    } elseif ($route.proxy_enabled -eq $true) {
      $failures.Add("sample disabled route must not be proxied") | Out-Null
    }
  }
  if ($services.Json) {
    $items = Get-JsonArray $services.Json "services"
    if (-not (Has-JsonItem $items "service_id" "sample-hello-metadata-service")) {
      $failures.Add("runtime services missing sample metadata service") | Out-Null
    }
  }
  if ($serviceDetail.Json -and [string]$serviceDetail.Json.service.lifecycle -ne "metadata") {
    $failures.Add("sample service detail should be metadata lifecycle") | Out-Null
  }
  if ($blockedPlan.Json) {
    $blockedBy = @($blockedPlan.Json.plan.blocked_by)
    if ($blockedPlan.Json.plan.can_apply -ne $false) {
      $failures.Add("sample metadata service plan-start should not be applyable") | Out-Null
    }
    if (-not ($blockedBy -contains "metadata lifecycle cannot start")) {
      $failures.Add("sample metadata plan-start missing lifecycle block") | Out-Null
    }
  }
  Invoke-Api "dynamic.proxy.sample.disabled.not-proxied" GET "/sample-hello/ping" -Token $script:AdminToken -Expected @(404) | Out-Null

  Invoke-Api "installer.disable.sample" POST "/admin/modules/ojos.sample-hello/disable" @{} -Token $script:AdminToken -Expected @(200) | Out-Null
  $disabledActive = Invoke-Api "runtime-snapshot.sample.disabled.active" GET "/admin/modules/runtime-snapshot" -Token $script:AdminToken -Expected @(200)
  $disabledAll = Invoke-Api "runtime-snapshot.sample.disabled.include-disabled" GET "/admin/modules/runtime-snapshot?include_disabled=true" -Token $script:AdminToken -Expected @(200)
  if ($disabledActive.Json) {
    $modules = Get-JsonArray $disabledActive.Json "modules"
    $permissions = Get-JsonArray $disabledActive.Json "permissions"
    if (Has-JsonItem $modules "module_id" "ojos.sample-hello") { $failures.Add("disabled sample should not appear in active snapshot") | Out-Null }
    if (Has-JsonItem $permissions "permission_key" "sample.hello.view") { $failures.Add("disabled sample permission should not be active") | Out-Null }
  }
  if ($disabledAll.Json) {
    $modules = Get-JsonArray $disabledAll.Json "modules"
    $permissions = Get-JsonArray $disabledAll.Json "permissions"
    if (-not (Has-JsonItem $modules "module_id" "ojos.sample-hello")) { $failures.Add("include_disabled missing disabled sample module") | Out-Null }
    if (-not (Has-JsonItem $permissions "permission_key" "sample.hello.view")) { $failures.Add("include_disabled missing sample permission") | Out-Null }
  }
  Invoke-Api "installer.uninstall-dry-run.sample" POST "/admin/modules/ojos.sample-hello/uninstall-dry-run" @{} -Token $script:AdminToken -Expected @(200) | Out-Null
  Invoke-Api "installer.install.sample.user.denied" POST "/admin/modules/install" $sampleManifest -Token $script:UserToken -Expected @(403) | Out-Null
  Invoke-Api "installer.install.sample.none" POST "/admin/modules/install" $sampleManifest -Expected @(401) | Out-Null
} finally {
  $results | ConvertTo-Json -Depth 10 | Set-Content (Join-Path $ReportDir "results.json")
  if ($leaks.Count -gt 0) {
    foreach ($leak in $leaks) { $failures.Add("path leak: $leak") | Out-Null }
  }
  $summary = [ordered]@{
    total = $results.Count
    failed = $failures.Count
    path_leaks = $leaks.Count
    sample_module_compat = if ($failures.Count -eq 0) { "passed" } else { "failed" }
  }
  $summary | ConvertTo-Json -Depth 5
  if ($failures.Count -gt 0) {
    $failures | Set-Content (Join-Path $ReportDir "failures.txt")
    throw "module compatibility failed: $($failures -join '; ')"
  }
}
