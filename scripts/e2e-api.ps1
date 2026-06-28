<#
用途:
  在 Docker Service Runtime 启动后，通过 Gateway 对当前 API 做真实运行时验收。

运行前提:
  1. 已在当前 PowerShell 进程或本地 .env 中准备必要环境变量。
  2. 已执行 docker compose --env-file .env -f deploy\compose\docker-compose.yml up -d --build。
  3. Postgres、Redis、Gateway、Auth、Problem API、Judge API 等服务可用。

运行方式:
  powershell -NoProfile -File scripts\e2e-api.ps1 `
    -BaseUrl http://localhost:8080/api `
    -AdminUsername admin1 -AdminPassword admin123 `
    -UserUsername user1 -UserPassword user123 `
    -WorkerToken $env:OJOS_WORKER_TOKEN

参数:
  -BaseUrl        Gateway API 基准地址，默认 http://localhost:8080/api。
  -AdminUsername 管理员测试账号用户名。
  -AdminPassword 管理员测试账号密码。
  -UserUsername  普通测试账号用户名。
  -UserPassword  普通测试账号密码。
  -WorkerToken   Worker Link 接口使用的 X-OJOS-Worker-Token。

输出位置:
  运行报告、响应摘要、token 运行文件只写入 .tmp/agent/reports/api-runtime/。

失败处理:
  任一接口状态码不符合预期、路径泄露扫描命中、Worker claim 未拿到任务时，脚本返回非零退出码。
#>
param(
  [string]$BaseUrl = "http://localhost:8080/api",
  [string]$AdminUsername = "admin1",
  [string]$AdminPassword = "admin123",
  [string]$UserUsername = "user1",
  [string]$UserPassword = "user123",
  [string]$WorkerToken = ""
)

$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"

$Root = (Resolve-Path ".").Path
$ComposeFile = "deploy\compose\docker-compose.yml"
$ComposeEnvFile = if (Test-Path (Join-Path $Root ".env")) { ".env" } else { ".env.example" }
$ReportDir = Join-Path $Root ".tmp/agent/reports/api-runtime"
$LogDir = Join-Path $Root ".tmp/agent/logs/api-runtime"

New-Item -ItemType Directory -Force $ReportDir, $LogDir | Out-Null

if (-not (Test-Path (Join-Path $Root $ComposeFile))) {
  throw "must run from project root; $ComposeFile not found"
}
if ([string]::IsNullOrWhiteSpace($WorkerToken)) {
  throw "WorkerToken is required"
}

$results = New-Object System.Collections.Generic.List[object]
$failures = New-Object System.Collections.Generic.List[string]
$tokens = [ordered]@{}

$script:AdminToken = $null
$script:UserToken = $null
$script:AdminId = 0
$script:UserId = 0
$script:ProblemId = 0
$script:SubmissionId = 0
$script:OtherSubmissionId = 0
$script:WorkerSubmissionId = 0
$script:WorkerId = "agent-worker-$(Get-Random)"
$script:ClaimedTask = $null
$script:AdminHealthStatus = ""
$script:AdminHealthJudgeStatus = ""
$script:LeakedPaths = New-Object System.Collections.Generic.List[string]

$forbiddenTokens = @(
  "code_path",
  "result_path",
  "package_dir",
  "stdout_path",
  "stderr_path",
  "checker_log_path",
  "D:\",
  "/data/",
  "/var/lib/",
  "/tmp/ojos",
  "/app/"
)

Add-Type -AssemblyName System.Net.Http
$script:HttpHandler = [System.Net.Http.HttpClientHandler]::new()
$script:HttpHandler.UseProxy = $false
$script:HttpClient = [System.Net.Http.HttpClient]::new($script:HttpHandler)
$script:HttpClient.Timeout = [TimeSpan]::FromSeconds(30)
$script:HttpClient.DefaultRequestHeaders.ExpectContinue = $false

function ConvertTo-JsonBody($obj) {
  if ($null -eq $obj) { return $null }
  return ($obj | ConvertTo-Json -Depth 20 -Compress)
}

function Invoke-Compose {
  param([Parameter(ValueFromRemainingArguments = $true)][object[]]$ComposeArgs)

  if ($ComposeArgs.Count -eq 1 -and $ComposeArgs[0] -is [array]) {
    $ComposeArgs = $ComposeArgs[0]
  }

  $staticEnv = @{
    POSTGRES_PASSWORD = "api-e2e-postgres-password"
    POSTGRES_DSN = "postgres://postgres:api-e2e-postgres-password@postgres:5432/ojos?sslmode=disable"
    JWT_SECRET = "api-e2e-jwt-secret"
    OJOS_WORKER_TOKEN = if ($WorkerToken) { $WorkerToken } else { "api-e2e-worker-token" }
    ROOT_RUNTIME_MANAGER_INTERNAL_TOKEN = "api-e2e-runtime-token"
  }
  $previous = @{}
  foreach ($name in $staticEnv.Keys) {
    $previous[$name] = [Environment]::GetEnvironmentVariable($name, "Process")
    if ([string]::IsNullOrWhiteSpace($previous[$name])) {
      [Environment]::SetEnvironmentVariable($name, $staticEnv[$name], "Process")
    }
  }

  try {
    $dockerArgs = @("compose", "--env-file", $ComposeEnvFile, "-f", $ComposeFile) + @($ComposeArgs)
    & docker @dockerArgs
  } finally {
    foreach ($name in $staticEnv.Keys) {
      [Environment]::SetEnvironmentVariable($name, $previous[$name], "Process")
    }
  }
}

function New-HttpMethod([string]$Method) {
  return [System.Net.Http.HttpMethod]::new($Method.ToUpperInvariant())
}

function Add-RequestHeaders($Request, [hashtable]$Headers) {
  foreach ($k in $Headers.Keys) {
    [void]$Request.Headers.TryAddWithoutValidation($k, [string]$Headers[$k])
  }
}

function Convert-HeadersToHashtable($Response) {
  $out = @{}
  foreach ($h in $Response.Headers.GetEnumerator()) { $out[$h.Key] = ($h.Value -join ",") }
  foreach ($h in $Response.Content.Headers.GetEnumerator()) { $out[$h.Key] = ($h.Value -join ",") }
  return $out
}

function Scan-Leak([string]$Name, [string]$Text) {
  if ([string]::IsNullOrEmpty($Text)) { return }
  foreach ($token in $forbiddenTokens) {
    if ($Text.Contains($token)) {
      $script:LeakedPaths.Add("$Name contains $token") | Out-Null
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
    [hashtable]$Headers = @{},
    [int[]]$Expected = @(200)
  )

  $uri = if ($Path.StartsWith("http")) { $Path } else { "$BaseUrl$Path" }
  $text = ""
  $json = $null
  $headersOut = @{}
  $sc = 0
  $request = $null
  $resp = $null

  try {
    $request = [System.Net.Http.HttpRequestMessage]::new((New-HttpMethod $Method), $uri)
    if ($Token) {
      $request.Headers.Authorization = [System.Net.Http.Headers.AuthenticationHeaderValue]::new("Bearer", $Token)
    }
    Add-RequestHeaders $request $Headers
    if ($null -ne $Body) {
      $jsonBody = ConvertTo-JsonBody $Body
      $request.Content = [System.Net.Http.StringContent]::new($jsonBody, [System.Text.Encoding]::UTF8, "application/json")
    }

    $resp = $script:HttpClient.SendAsync($request).GetAwaiter().GetResult()
    $sc = [int]$resp.StatusCode
    $text = $resp.Content.ReadAsStringAsync().GetAwaiter().GetResult()
    $headersOut = Convert-HeadersToHashtable $resp
    if ($text.Trim().Length -gt 0) {
      try { $json = $text | ConvertFrom-Json } catch {}
    }
  } catch {
    $text = $_.Exception.Message
    $sc = 0
  } finally {
    if ($request) { $request.Dispose() }
    if ($resp) { $resp.Dispose() }
  }

  $ok = $Expected -contains [int]$sc
  $summary = $text
  if ($summary.Length -gt 700) { $summary = $summary.Substring(0, 700) + "..." }
  $results.Add([pscustomobject]@{
    Name = $Name
    Method = $Method
    Path = $Path
    Status = [int]$sc
    Expected = ($Expected -join ",")
    Ok = $ok
    Summary = $summary
  }) | Out-Null
  if (-not $ok) {
    $failures.Add("$Name expected $($Expected -join '/') got $sc $summary") | Out-Null
  }
  Scan-Leak -Name $Name -Text $text

  return [pscustomobject]@{
    Status = [int]$sc
    Text = $text
    Json = $json
    Ok = $ok
    Headers = $headersOut
  }
}

function Invoke-Download {
  param(
    [string]$Name,
    [string]$Path,
    [hashtable]$Headers = @{},
    [int[]]$Expected = @(200)
  )

  $uri = if ($Path.StartsWith("http")) { $Path } else { "$BaseUrl$Path" }
  $sc = 0
  $content = ""
  $respHeaders = @{}
  $request = $null
  $resp = $null

  try {
    $request = [System.Net.Http.HttpRequestMessage]::new([System.Net.Http.HttpMethod]::Get, $uri)
    Add-RequestHeaders $request $Headers
    $resp = $script:HttpClient.SendAsync($request).GetAwaiter().GetResult()
    $sc = [int]$resp.StatusCode
    $bytes = $resp.Content.ReadAsByteArrayAsync().GetAwaiter().GetResult()
    $content = [System.Text.Encoding]::UTF8.GetString($bytes)
    $respHeaders = Convert-HeadersToHashtable $resp
  } catch {
    $content = $_.Exception.Message
    $sc = 0
  } finally {
    if ($request) { $request.Dispose() }
    if ($resp) { $resp.Dispose() }
  }

  $ok = $Expected -contains [int]$sc
  $sha = $respHeaders["X-OJOS-Artifact-Sha256"]
  $size = $respHeaders["X-OJOS-Artifact-Size"]
  $summary = "sha256=$sha size=$size bytes=$($content.Length)"
  $results.Add([pscustomobject]@{
    Name = $Name
    Method = "GET"
    Path = $Path
    Status = [int]$sc
    Expected = ($Expected -join ",")
    Ok = $ok
    Summary = $summary
  }) | Out-Null
  if (-not $ok) {
    $failures.Add("$Name expected $($Expected -join '/') got $sc $content") | Out-Null
  }
  Scan-Leak -Name $Name -Text $content

  return [pscustomobject]@{
    Status = [int]$sc
    Content = $content
    Headers = $respHeaders
    Ok = $ok
  }
}

function Write-Report($name, [string[]]$patterns) {
  $path = Join-Path $ReportDir $name
  "# $name" | Set-Content $path
  "" | Add-Content $path
  $selected = $results | Where-Object {
    $n = $_.Name
    $patterns | Where-Object { $n -like $_ }
  }
  foreach ($r in $selected) {
    $mark = if ($r.Ok) { "PASS" } else { "FAIL" }
    $endpoint = "$($r.Method) $($r.Path)"
    $cleanSummary = [string]$r.Summary
    $cleanSummary = $cleanSummary.Replace("`r", "").Replace("`n", " ")
    "- [$mark] $($r.Name) ``$endpoint`` status=$($r.Status) expected=$($r.Expected)" | Add-Content $path
    "  summary: $cleanSummary" | Add-Content $path
  }
}

function Get-JsonArray($Value, [string]$Property) {
  if ($null -eq $Value) { return @() }
  $prop = $Value.PSObject.Properties[$Property]
  if ($null -eq $prop) { return @() }
  return @($prop.Value)
}

function Has-JsonItem($Items, [string]$Property, [string]$Expected) {
  foreach ($item in @($Items)) {
    if ($null -eq $item) { continue }
    $prop = $item.PSObject.Properties[$Property]
    if ($null -ne $prop -and [string]$prop.Value -eq $Expected) {
      return $true
    }
  }
  return $false
}

function Get-JsonItem($Items, [string]$Property, [string]$Expected) {
  foreach ($item in @($Items)) {
    $prop = $item.PSObject.Properties[$Property]
    if ($null -ne $prop -and [string]$prop.Value -eq $Expected) {
      return $item
    }
  }
  return $null
}

function Ensure-AdminRole {
  param([int64]$UserId)
  $sql = "insert into user_roles(user_id, role_id) select $UserId, id from roles where name='super_admin' on conflict do nothing;"
  Invoke-Compose @("exec", "-T", "postgres", "psql", "-v", "ON_ERROR_STOP=1", "-U", "postgres", "-d", "ojos", "-c", $sql) | Out-Null
}

function Restart-ComposeWorker([bool]$Start) {
  if ($Start) {
    Invoke-Compose @("start", "judge-worker") | Out-Null
  } else {
    Invoke-Compose @("stop", "judge-worker") | Out-Null
  }
}

function Wait-GatewayReady {
  param([int]$TimeoutSeconds = 30)

  $deadline = (Get-Date).AddSeconds($TimeoutSeconds)
  while ((Get-Date) -lt $deadline) {
    try {
      $request = [System.Net.Http.HttpRequestMessage]::new([System.Net.Http.HttpMethod]::Get, "$BaseUrl/health")
      $resp = $script:HttpClient.SendAsync($request).GetAwaiter().GetResult()
      $status = [int]$resp.StatusCode
      $request.Dispose()
      $resp.Dispose()
      if ($status -lt 500) { return }
    } catch {
      Start-Sleep -Milliseconds 500
      continue
    }
    Start-Sleep -Milliseconds 500
  }

  throw "gateway did not become ready within $TimeoutSeconds seconds"
}

try {
  Invoke-Compose @("ps") | Set-Content (Join-Path $LogDir "compose-ps.txt")
  Invoke-Compose @("logs", "--tail=200") | Set-Content (Join-Path $LogDir "compose-logs.txt")

  $r = Invoke-Api "auth.register.admin" POST "/auth/register" @{ username = $AdminUsername; password = $AdminPassword } -Expected @(200)
  $r = Invoke-Api "auth.register.user" POST "/auth/register" @{ username = $UserUsername; password = $UserPassword } -Expected @(200)
  $r = Invoke-Api "auth.login.admin.pregrant" POST "/auth/login" @{ username = $AdminUsername; password = $AdminPassword } -Expected @(200)
  if ($r.Json.code -eq 0) {
    $script:AdminId = [int64]$r.Json.data.user_id
    Ensure-AdminRole $script:AdminId
  }
  $r = Invoke-Api "auth.login.admin" POST "/auth/login" @{ username = $AdminUsername; password = $AdminPassword } -Expected @(200)
  if ($r.Json.code -eq 0) {
    $script:AdminToken = $r.Json.data.token
    $script:AdminId = [int64]$r.Json.data.user_id
    $tokens.admin = $script:AdminToken
  }
  $r = Invoke-Api "auth.login.user" POST "/auth/login" @{ username = $UserUsername; password = $UserPassword } -Expected @(200)
  if ($r.Json.code -eq 0) {
    $script:UserToken = $r.Json.data.token
    $script:UserId = [int64]$r.Json.data.user_id
    $tokens.user = $script:UserToken
  }
  ($tokens | ConvertTo-Json -Depth 5) | Set-Content (Join-Path $ReportDir "tokens.local.json")

  Invoke-Api "auth.profile.admin" GET "/auth/profile" -Token $script:AdminToken -Expected @(200) | Out-Null
  Invoke-Api "auth.profile.none" GET "/auth/profile" -Expected @(401) | Out-Null
  Invoke-Api "auth.me.user" GET "/auth/me" -Token $script:UserToken -Expected @(200) | Out-Null
  Invoke-Api "auth.admin.users.admin" GET "/auth/admin/users" -Token $script:AdminToken -Expected @(200) | Out-Null
  Invoke-Api "auth.admin.users.user" GET "/auth/admin/users" -Token $script:UserToken -Expected @(403) | Out-Null
  Invoke-Api "auth.admin.users.none" GET "/auth/admin/users" -Expected @(401) | Out-Null
  Invoke-Api "auth.admin.roles.admin" GET "/auth/admin/roles" -Token $script:AdminToken -Expected @(200) | Out-Null
  Invoke-Api "auth.admin.roles.user" GET "/auth/admin/roles" -Token $script:UserToken -Expected @(403) | Out-Null
  Invoke-Api "auth.admin.permissions.admin" GET "/auth/admin/permissions" -Token $script:AdminToken -Expected @(200) | Out-Null
  Invoke-Api "auth.admin.permissions.user" GET "/auth/admin/permissions" -Token $script:UserToken -Expected @(403) | Out-Null
  Invoke-Api "auth.admin.users.roles.add" POST "/auth/admin/users/roles" @{ user_id = $script:UserId; role = "problem_viewer" } -Token $script:AdminToken -Expected @(200) | Out-Null
  Invoke-Api "auth.admin.users.roles.delete" DELETE "/auth/admin/users/roles" @{ user_id = $script:UserId; role = "problem_viewer" } -Token $script:AdminToken -Expected @(200) | Out-Null
  Invoke-Api "auth.admin.permission-check" POST "/auth/admin/permission-check" @{ user_id = $script:UserId; permission = "problem.view"; scope_type = "system"; scope_id = 0 } -Token $script:AdminToken -Expected @(200) | Out-Null
  Invoke-Api "auth.admin.audit-logs" GET "/auth/admin/audit-logs" -Token $script:AdminToken -Expected @(200) | Out-Null

  $slug = "agent-ab-$(Get-Random)"
  $r = Invoke-Api "problem.create.admin" POST "/problem/problems" @{
    title = "Agent A+B"
    slug = $slug
    statement = "Read two integers and print their sum."
    visibility = "public"
    difficulty = "easy"
    tags = "math,io"
    time_limit_ms = 1000
    memory_limit_mb = 128
  } -Token $script:AdminToken -Expected @(200)
  if ($r.Json.problem_id) { $script:ProblemId = [int64]$r.Json.problem_id }

  Invoke-Api "problem.create.user.denied" POST "/problem/problems" @{ title = "Denied"; slug = "denied-$(Get-Random)" } -Token $script:UserToken -Expected @(403) | Out-Null
  Invoke-Api "problem.create.none" POST "/problem/problems" @{ title = "Denied" } -Expected @(401) | Out-Null
  Invoke-Api "problem.list.user" GET "/problem/problems?page=1&page_size=20" -Token $script:UserToken -Expected @(200) | Out-Null
  Invoke-Api "problem.detail.user" GET "/problem/problems/$script:ProblemId" -Token $script:UserToken -Expected @(200) | Out-Null
  Invoke-Api "problem.detail.notfound" GET "/problem/problems/999999999" -Token $script:UserToken -Expected @(404) | Out-Null
  Invoke-Api "problem.update.admin" PUT "/problem/problems/$script:ProblemId" @{ title = "Agent A+B Updated"; status = "published"; visibility = "public" } -Token $script:AdminToken -Expected @(200) | Out-Null
  Invoke-Api "problem.update.user.denied" PUT "/problem/problems/$script:ProblemId" @{ title = "Nope" } -Token $script:UserToken -Expected @(403) | Out-Null
  Invoke-Api "problem.testcase.add.admin" POST "/problem/problems/$script:ProblemId/test-cases" @{ case_no = 1; input = "1 2`n"; answer = "3`n"; score = 100; sample = $true; hidden = $false } -Token $script:AdminToken -Expected @(200) | Out-Null
  Invoke-Api "problem.testcase.list.admin" GET "/problem/problems/$script:ProblemId/test-cases" -Token $script:AdminToken -Expected @(200) | Out-Null
  Invoke-Api "problem.testcase.list.user.denied" GET "/problem/problems/$script:ProblemId/test-cases" -Token $script:UserToken -Expected @(403) | Out-Null
  Invoke-Api "problem.testcase.update.admin" PUT "/problem/problems/$script:ProblemId/test-cases/1" @{ input = "2 5`n"; answer = "7`n"; score = 100; sample = $true; hidden = $false } -Token $script:AdminToken -Expected @(200) | Out-Null
  Invoke-Api "problem.package.get.admin" GET "/problem/problems/$script:ProblemId/package" -Token $script:AdminToken -Expected @(200) | Out-Null
  Invoke-Api "problem.package.validate.admin" POST "/problem/problems/$script:ProblemId/package/validate" @{} -Token $script:AdminToken -Expected @(200) | Out-Null
  Invoke-Api "problem.package.cases.admin" GET "/problem/problems/$script:ProblemId/package/cases" -Token $script:AdminToken -Expected @(200) | Out-Null
  Invoke-Api "problem.testcase.delete.admin" DELETE "/problem/problems/$script:ProblemId/test-cases/1" -Token $script:AdminToken -Expected @(200) | Out-Null
  Invoke-Api "problem.testcase.add.admin.second" POST "/problem/problems/$script:ProblemId/test-cases" @{ case_no = 1; input = "1 2`n"; answer = "3`n"; score = 100; sample = $true; hidden = $false } -Token $script:AdminToken -Expected @(200) | Out-Null

  Invoke-Api "judge.languages.user" GET "/judge/languages" -Token $script:UserToken -Expected @(200) | Out-Null
  $r = Invoke-Api "judge.submission.create.user" POST "/judge/submissions" @{ problem_id = $script:ProblemId; language = "python3"; code = "a,b=map(int,input().split()); print(a+b)" } -Token $script:UserToken -Expected @(200)
  if ($r.Json.submission_id) { $script:SubmissionId = [int64]$r.Json.submission_id }
  $r = Invoke-Api "judge.submission.create.admin.other" POST "/judge/submissions" @{ problem_id = $script:ProblemId; language = "python3"; code = "print(3)" } -Token $script:AdminToken -Expected @(200)
  if ($r.Json.submission_id) { $script:OtherSubmissionId = [int64]$r.Json.submission_id }
  Invoke-Api "judge.submissions.list.user" GET "/judge/submissions?page=1&page_size=20" -Token $script:UserToken -Expected @(200) | Out-Null
  Invoke-Api "judge.submission.detail.user.own" GET "/judge/submissions/$script:SubmissionId" -Token $script:UserToken -Expected @(200) | Out-Null
  Invoke-Api "judge.submission.detail.user.other.denied" GET "/judge/submissions/$script:OtherSubmissionId" -Token $script:UserToken -Expected @(403) | Out-Null
  Invoke-Api "judge.submission.detail.admin" GET "/judge/submissions/$script:SubmissionId" -Token $script:AdminToken -Expected @(200) | Out-Null
  Invoke-Api "judge.submission.cases.user" GET "/judge/submissions/$script:SubmissionId/cases" -Token $script:UserToken -Expected @(200) | Out-Null
  Invoke-Api "judge.submission.debug.user.denied" GET "/judge/submissions/$script:SubmissionId/debug-logs" -Token $script:UserToken -Expected @(403, 404) | Out-Null
  Invoke-Api "judge.submission.debug.admin" GET "/judge/submissions/$script:SubmissionId/debug-logs" -Token $script:AdminToken -Expected @(200, 404) | Out-Null
  Invoke-Api "judge.submission.cancel.user.denied" POST "/judge/submissions/$script:SubmissionId/cancel" @{ reason = "test" } -Token $script:UserToken -Expected @(403) | Out-Null
  Invoke-Api "judge.problem.rejudge.user.denied" POST "/judge/problems/$script:ProblemId/rejudge" @{} -Token $script:UserToken -Expected @(403) | Out-Null
  Invoke-Api "judge.problem.rejudge.admin" POST "/judge/problems/$script:ProblemId/rejudge" @{} -Token $script:AdminToken -Expected @(200) | Out-Null

  Restart-ComposeWorker $false
  try {
    Start-Sleep -Seconds 2
    $r = Invoke-Api "judge.submission.create.user.for-worker-claim" POST "/judge/submissions" @{ problem_id = $script:ProblemId; language = "python3"; code = "a,b=map(int,input().split()); print(a+b)" } -Token $script:UserToken -Expected @(200)
    if ($r.Json.submission_id) { $script:WorkerSubmissionId = [int64]$r.Json.submission_id }
    Invoke-Api "worker.register.none" POST "/judge/worker/register" @{ worker_id = "bad"; max_concurrency = 1 } -Expected @(401) | Out-Null
    Invoke-Api "worker.register.bad-token" POST "/judge/worker/register" @{ worker_id = "bad"; max_concurrency = 1 } -Headers @{ "X-OJOS-Worker-Token" = "wrong" } -Expected @(401) | Out-Null
    Invoke-Api "worker.register.ok" POST "/judge/worker/register" @{
      worker_id = $script:WorkerId
      worker_name = "Agent Worker"
      hostname = "agent"
      version = "e2e-api"
      capabilities = @("probe")
      supported_languages = @("python3", "cpp17", "c11", "java17")
      max_concurrency = 1
    } -Headers @{ "X-OJOS-Worker-Token" = $WorkerToken } -Expected @(200) | Out-Null
    Invoke-Api "worker.heartbeat.ok" POST "/judge/worker/heartbeat" @{ worker_id = $script:WorkerId; running_tasks = @(); running_count = 0; available_slots = 1 } -Headers @{ "X-OJOS-Worker-Token" = $WorkerToken } -Expected @(200) | Out-Null
    Invoke-Api "worker.claim.task" POST "/judge/worker/tasks/claim" @{ worker_id = $script:WorkerId; capabilities = @("probe"); supported_languages = @("python3", "cpp17", "c11", "java17"); available_slots = 1 } -Headers @{ "X-OJOS-Worker-Token" = $WorkerToken } -Expected @(200) | ForEach-Object {
      if ($_.Json.tasks -and $_.Json.tasks.Count -gt 0) {
        $script:ClaimedTask = $_.Json.tasks[0]
      }
    }
    if ($script:ClaimedTask) {
      Invoke-Api "worker.task.heartbeat.ok" POST "/judge/worker/tasks/$($script:ClaimedTask.task_id)/heartbeat" @{ worker_id = $script:WorkerId; lease_version = [int]$script:ClaimedTask.lease_version } -Headers @{ "X-OJOS-Worker-Token" = $WorkerToken } -Expected @(200) | Out-Null
      Invoke-Download "worker.artifact.source.ok" $script:ClaimedTask.source.url -Headers @{ "X-OJOS-Worker-Token" = $WorkerToken } -Expected @(200) | Out-Null
      Invoke-Download "worker.artifact.package.ok" $script:ClaimedTask.problem_package.url -Headers @{ "X-OJOS-Worker-Token" = $WorkerToken } -Expected @(200) | Out-Null
      Invoke-Api "worker.result.wrong-lease.denied" POST "/judge/worker/tasks/$($script:ClaimedTask.task_id)/result" @{ worker_id = $script:WorkerId; lease_version = 999999; status = "ACCEPTED"; score = 100; time_ms = 1; memory_kb = 1; message = "wrong lease"; cases = @() } -Headers @{ "X-OJOS-Worker-Token" = $WorkerToken } -Expected @(400, 403, 404) | Out-Null
      Invoke-Api "worker.fail.ok" POST "/judge/worker/tasks/$($script:ClaimedTask.task_id)/fail" @{ worker_id = $script:WorkerId; lease_version = [int]$script:ClaimedTask.lease_version; error_type = "SYSTEM"; message = "e2e cleanup"; retryable = $false } -Headers @{ "X-OJOS-Worker-Token" = $WorkerToken } -Expected @(200) | Out-Null
    } else {
      $failures.Add("worker claim did not return a task after pending submissions") | Out-Null
    }
  } finally {
    Restart-ComposeWorker $true
    Wait-GatewayReady
  }

  Invoke-Api "admin.judge.queue.admin" GET "/judge/admin/queue" -Token $script:AdminToken -Expected @(200) | Out-Null
  Invoke-Api "admin.judge.queue.user" GET "/judge/admin/queue" -Token $script:UserToken -Expected @(403) | Out-Null
  Invoke-Api "admin.judge.queue.none" GET "/judge/admin/queue" -Expected @(401) | Out-Null
  Invoke-Api "admin.judge.workers.admin" GET "/judge/admin/workers" -Token $script:AdminToken -Expected @(200) | Out-Null
  Invoke-Api "admin.judge.workers.user" GET "/judge/admin/workers" -Token $script:UserToken -Expected @(403) | Out-Null
  Invoke-Api "admin.judge.tasks.admin" GET "/judge/admin/tasks" -Token $script:AdminToken -Expected @(200) | Out-Null
  Invoke-Api "admin.judge.tasks.user" GET "/judge/admin/tasks" -Token $script:UserToken -Expected @(403) | Out-Null
  Invoke-Api "admin.judge.drain.admin" POST "/judge/admin/workers/$script:WorkerId/drain" @{} -Token $script:AdminToken -Expected @(200, 404) | Out-Null
  Invoke-Api "admin.judge.requeue.admin" POST "/judge/admin/submissions/$script:SubmissionId/requeue" @{} -Token $script:AdminToken -Expected @(200, 400, 404) | Out-Null
  Invoke-Api "admin.judge.requeue.user" POST "/judge/admin/submissions/$script:SubmissionId/requeue" @{} -Token $script:UserToken -Expected @(403) | Out-Null

  $adminHealth = Invoke-Api "admin.health.admin" GET "/admin/health" -Token $script:AdminToken -Expected @(200)
  if ($adminHealth.Json) {
    $script:AdminHealthStatus = [string]$adminHealth.Json.status
    $judgeHealth = @($adminHealth.Json.components | Where-Object { $_.name -eq "judge" } | Select-Object -First 1)
    if ($judgeHealth.Count -gt 0) {
      $script:AdminHealthJudgeStatus = [string]$judgeHealth[0].status
    }
    if ($script:AdminHealthStatus -ne "ok") {
      $failures.Add("admin health overall status expected ok got $script:AdminHealthStatus") | Out-Null
    }
    if ($script:AdminHealthJudgeStatus -ne "ok") {
      $failures.Add("admin health judge component expected ok got $script:AdminHealthJudgeStatus") | Out-Null
    }
  } else {
    $failures.Add("admin health response is not JSON") | Out-Null
  }
  Invoke-Api "admin.health.user" GET "/admin/health" -Token $script:UserToken -Expected @(403) | Out-Null
  Invoke-Api "admin.health.none" GET "/admin/health" -Expected @(401) | Out-Null

  Invoke-Api "modules.removed.admin" GET "/admin/modules" -Token $script:AdminToken -Expected @(404, 410) | Out-Null
  Invoke-Api "modules.removed.none" GET "/admin/modules" -Expected @(401, 404, 410) | Out-Null
  Invoke-Api "services.list.admin" GET "/admin/services" -Token $script:AdminToken -Expected @(200) | Out-Null
  Invoke-Api "services.list.user" GET "/admin/services" -Token $script:UserToken -Expected @(403) | Out-Null
  Invoke-Api "services.list.none" GET "/admin/services" -Expected @(401) | Out-Null
  Invoke-Api "services.sets.admin" GET "/admin/sets" -Token $script:AdminToken -Expected @(200) | Out-Null
  Invoke-Api "services.sets.user" GET "/admin/sets" -Token $script:UserToken -Expected @(403) | Out-Null
  $topologyResp = Invoke-Api "services.topology.admin" GET "/admin/topology" -Token $script:AdminToken -Expected @(200)
  if ($topologyResp.Json) {
    if ((Get-JsonArray $topologyResp.Json "nodes").Count -le 0) {
      $failures.Add("service topology runtime nodes expected non-empty") | Out-Null
    }
    if ((Get-JsonArray $topologyResp.Json "service_nodes").Count -le 0) {
      $failures.Add("service topology service_nodes expected non-empty") | Out-Null
    }
  }
  $runtimeSnapshot = Invoke-Api "services.runtime-snapshot.admin" GET "/admin/runtime/snapshot" -Token $script:AdminToken -Expected @(200)
  $runtimeSnapshotAll = Invoke-Api "services.runtime-snapshot.admin.include-disabled" GET "/admin/runtime/snapshot?include_disabled=true" -Token $script:AdminToken -Expected @(200)
  Invoke-Api "services.runtime-snapshot.user" GET "/admin/runtime/snapshot" -Token $script:UserToken -Expected @(403) | Out-Null
  Invoke-Api "services.runtime-snapshot.none" GET "/admin/runtime/snapshot" -Expected @(401) | Out-Null
  $runtimeRoutes = Invoke-Api "services.runtime.routes.admin" GET "/admin/runtime/routes" -Token $script:AdminToken -Expected @(200)
  Invoke-Api "services.runtime.routes.user" GET "/admin/runtime/routes" -Token $script:UserToken -Expected @(403) | Out-Null
  Invoke-Api "services.runtime.routes.none" GET "/admin/runtime/routes" -Expected @(401) | Out-Null
  $runtimeReload = Invoke-Api "services.runtime.reload.admin" POST "/admin/runtime/reload" @{} -Token $script:AdminToken -Expected @(200)
  Invoke-Api "services.runtime.reload.user" POST "/admin/runtime/reload" @{} -Token $script:UserToken -Expected @(403) | Out-Null
  Invoke-Api "services.runtime.reload.none" POST "/admin/runtime/reload" @{} -Expected @(401) | Out-Null
  $runtimeServices = Invoke-Api "runtime.services.admin" GET "/admin/runtime/services" -Token $script:AdminToken -Expected @(200)
  Invoke-Api "runtime.services.user" GET "/admin/runtime/services" -Token $script:UserToken -Expected @(403) | Out-Null
  Invoke-Api "runtime.services.none" GET "/admin/runtime/services" -Expected @(401) | Out-Null
  $runtimeProblemService = Invoke-Api "runtime.service.problem-api.admin" GET "/admin/runtime/services/problem-api" -Token $script:AdminToken -Expected @(200)
  $runtimePlanRestart = Invoke-Api "runtime.service.problem-api.plan-restart" POST "/admin/runtime/services/problem-api/plan-restart" @{} -Token $script:AdminToken -Expected @(200)
  Invoke-Api "runtime.service.problem-api.plan-restart.user" POST "/admin/runtime/services/problem-api/plan-restart" @{} -Token $script:UserToken -Expected @(403) | Out-Null
  Invoke-Api "runtime.service.problem-api.plan-restart.none" POST "/admin/runtime/services/problem-api/plan-restart" @{} -Expected @(401) | Out-Null
  Invoke-Api "runtime.apply.gateway.disabled.admin" POST "/admin/runtime/plans/runtime-restart-problem-api/apply" @{} -Token $script:AdminToken -Expected @(501) | Out-Null
  Invoke-Api "runtime.apply.gateway.disabled.user" POST "/admin/runtime/plans/runtime-restart-problem-api/apply" @{} -Token $script:UserToken -Expected @(403) | Out-Null
  Invoke-Api "runtime.apply.gateway.disabled.none" POST "/admin/runtime/plans/runtime-restart-problem-api/apply" @{} -Expected @(401) | Out-Null
  Invoke-Api "runtime.operations.admin" GET "/admin/runtime/operations" -Token $script:AdminToken -Expected @(200) | Out-Null
  Invoke-Api "runtime.operations.user" GET "/admin/runtime/operations" -Token $script:UserToken -Expected @(403) | Out-Null
  Invoke-Api "runtime.operations.none" GET "/admin/runtime/operations" -Expected @(401) | Out-Null
  if ($runtimeSnapshot.Json) {
    $snapshotServiceNodes = Get-JsonArray $runtimeSnapshot.Json "service_nodes"
    if ($snapshotServiceNodes.Count -le 0) {
      $failures.Add("runtime snapshot service_nodes expected non-empty") | Out-Null
    }
    if (-not $runtimeSnapshot.Json.topology -or -not $runtimeSnapshot.Json.topology.nodes -or $runtimeSnapshot.Json.topology.nodes.Count -le 0) {
      $failures.Add("runtime snapshot topology nodes expected non-empty") | Out-Null
    }
    $snapshotServices = Get-JsonArray $runtimeSnapshot.Json "services"
    $snapshotWorkers = Get-JsonArray $runtimeSnapshot.Json "workers"
    if (-not (Has-JsonItem $snapshotServices "service_id" "problem-api")) {
      $failures.Add("runtime snapshot services missing problem-api") | Out-Null
    }
    if (-not (Has-JsonItem $snapshotServices "service_id" "judge-api")) {
      $failures.Add("runtime snapshot services missing judge-api") | Out-Null
    }
    if (-not (Has-JsonItem $snapshotWorkers "service_id" "judge-worker")) {
      $failures.Add("runtime snapshot workers missing judge-worker") | Out-Null
    }
    $snapshotTopologyNodes = Get-JsonArray $runtimeSnapshot.Json.topology "nodes"
    if (-not (Has-JsonItem $snapshotTopologyNodes "id" "problem-api:service:problem-api")) {
      $failures.Add("runtime topology missing service node problem-api") | Out-Null
    }
    if (-not (Has-JsonItem $snapshotTopologyNodes "id" "judge-worker:worker:judge-worker")) {
      $failures.Add("runtime topology missing worker node judge-worker") | Out-Null
    }
    $snapshotServiceIds = @($snapshotServiceNodes | Select-Object -ExpandProperty service_id)
    foreach ($expectedService in @("root-runtime-manager", "gateway", "web-shell", "problem-api", "judge-api", "judge-worker", "storage", "postgres")) {
      if ($snapshotServiceIds -notcontains $expectedService) {
        $failures.Add("runtime snapshot missing $expectedService") | Out-Null
      }
    }
    $permissionItems = Get-JsonArray $runtimeSnapshot.Json "permissions"
    if (-not (Has-JsonItem $permissionItems "permission_key" "problem.view")) {
      $failures.Add("runtime snapshot permission registry missing problem.view") | Out-Null
    }
  } else {
    $failures.Add("runtime snapshot response is not JSON") | Out-Null
  }
  if ($runtimeRoutes.Json) {
    $routeItems = Get-JsonArray $runtimeRoutes.Json "routes"
    if (-not (Has-JsonItem $routeItems "prefix" "/api/problem")) {
      $failures.Add("runtime route table missing /api/problem") | Out-Null
    }
    $problemRoute = Get-JsonItem $routeItems "prefix" "/api/problem"
    if ($null -eq $problemRoute) {
      $failures.Add("runtime route table missing /api/problem route") | Out-Null
    } else {
      if ([string]$problemRoute.service_id -ne "problem-api") {
        $failures.Add("runtime route /api/problem expected service_id problem-api got $($problemRoute.service_id)") | Out-Null
      }
      if ($problemRoute.proxy_enabled -ne $true) {
        $failures.Add("runtime route /api/problem expected proxy_enabled=true") | Out-Null
      }
      if ([string]$problemRoute.auth_mode -ne "user") {
        $failures.Add("runtime route /api/problem expected auth_mode=user got $($problemRoute.auth_mode)") | Out-Null
      }
      if ([string]$problemRoute.service_state -ne "RUNNING") {
        $failures.Add("runtime route /api/problem expected service_state RUNNING got $($problemRoute.service_state)") | Out-Null
      }
      if ([string]$problemRoute.service_health -ne "ok") {
        $failures.Add("runtime route /api/problem expected service_health ok got $($problemRoute.service_health)") | Out-Null
      }
      if ($problemRoute.PSObject.Properties["upstream_base"] -and -not [string]::IsNullOrWhiteSpace([string]$problemRoute.upstream_base)) {
        $failures.Add("runtime route table should not expose upstream_base by default") | Out-Null
      }
    }
    $judgeRoute = Get-JsonItem $routeItems "prefix" "/api/judge"
    if ($null -eq $judgeRoute) {
      $failures.Add("runtime route table missing /api/judge route") | Out-Null
    } else {
      if ($judgeRoute.proxy_enabled -eq $true) {
        $failures.Add("runtime route /api/judge should not proxy because it would cover reserved /api/judge/worker") | Out-Null
      }
      $judgeBlockedBy = @($judgeRoute.blocked_by)
      if ($judgeBlockedBy -notcontains "reserved prefix") {
        $failures.Add("runtime route /api/judge expected blocked_by reserved prefix got $($judgeBlockedBy -join ',')") | Out-Null
      }
    }
  } else {
    $failures.Add("runtime route table response is not JSON") | Out-Null
  }
  if ($runtimeServices.Json) {
    $runtimeServiceItems = Get-JsonArray $runtimeServices.Json "services"
    $runtimeWorkerItems = Get-JsonArray $runtimeServices.Json "workers"
    if (-not (Has-JsonItem $runtimeServiceItems "service_id" "problem-api")) {
      $failures.Add("runtime services API missing problem-api") | Out-Null
    }
    if (-not (Has-JsonItem $runtimeWorkerItems "service_id" "judge-worker")) {
      $failures.Add("runtime services API missing judge-worker") | Out-Null
    }
  } else {
    $failures.Add("runtime services response is not JSON") | Out-Null
  }
  if ($runtimeProblemService.Json) {
    if ([string]$runtimeProblemService.Json.service.service_id -ne "problem-api") {
      $failures.Add("runtime service detail expected problem-api got $($runtimeProblemService.Json.service.service_id)") | Out-Null
    }
    if ([string]$runtimeProblemService.Json.service.state -ne "RUNNING") {
      $failures.Add("runtime service problem-api expected RUNNING got $($runtimeProblemService.Json.service.state)") | Out-Null
    }
  } else {
    $failures.Add("runtime service problem-api response is not JSON") | Out-Null
  }
  if ($runtimePlanRestart.Json) {
    $plan = $runtimePlanRestart.Json.plan
    if ([string]$plan.service_id -ne "problem-api") {
      $failures.Add("runtime plan restart expected problem-api got $($plan.service_id)") | Out-Null
    }
    if ($plan.can_apply -ne $true) {
      $failures.Add("runtime plan restart should be operator applyable can_apply=true") | Out-Null
    }
    if ($plan.apply_enabled -ne $false) {
      $failures.Add("runtime plan restart should keep Gateway/Web apply_enabled=false") | Out-Null
    }
    if ([string]::IsNullOrWhiteSpace([string]$plan.operation_id)) {
      $failures.Add("runtime plan restart expected operation_id") | Out-Null
    }
    if ([string]::IsNullOrWhiteSpace([string]$plan.expires_at)) {
      $failures.Add("runtime plan restart expected expires_at") | Out-Null
    }
    $commands = @($plan.commands)
    if ($commands.Count -ne 1 -or [string]$commands[0].kind -ne "compose") {
      $failures.Add("runtime plan restart expected one compose command") | Out-Null
    } else {
      $argv = @($commands[0].argv)
      if ($argv -notcontains "docker" -or $argv -notcontains "compose" -or $argv -notcontains "restart" -or $argv -notcontains "problem-api") {
        $failures.Add("runtime plan restart command should contain docker compose restart problem-api got $($argv -join ',')") | Out-Null
      }
      if (($argv -join " ") -match "[;&|]") {
        $failures.Add("runtime plan restart command argv must not contain shell metacharacters") | Out-Null
      }
    }
  } else {
    $failures.Add("runtime plan restart response is not JSON") | Out-Null
  }
  if ($runtimeReload.Json) {
    if ($runtimeReload.Json.reloaded -ne $true) {
      $failures.Add("runtime reload response expected reloaded=true") | Out-Null
    }
  } else {
    $failures.Add("runtime reload response is not JSON") | Out-Null
  }
  Invoke-Api "dynamic.proxy.problem.list.user" GET "/problem/problems?page=1&page_size=1" -Token $script:UserToken -Expected @(200) | Out-Null
  Invoke-Api "services.detail.problem-api" GET "/admin/services/problem-api" -Token $script:AdminToken -Expected @(200) | Out-Null
  $composeRows = @(Invoke-Compose @("ps", "--format", "json") | ForEach-Object { $_ | ConvertFrom-Json })
  $internalServices = @(
    [pscustomobject]@{ Service = "auth"; Port = 8081 },
    [pscustomobject]@{ Service = "judge-api"; Port = 8082 },
    [pscustomobject]@{ Service = "problem-api"; Port = 8083 },
    [pscustomobject]@{ Service = "root-runtime-manager"; Port = 8090 },
    [pscustomobject]@{ Service = "postgres"; Port = 5432 },
    [pscustomobject]@{ Service = "redis"; Port = 6379 }
  )
  foreach ($item in $internalServices) {
    $p = [int]$item.Port
    $svc = [string]$item.Service
    $row = $composeRows | Where-Object { $_.Service -eq $svc } | Select-Object -First 1
    $published = @()
    if ($row -and $row.Publishers) {
      $published = @($row.Publishers | Where-Object { [int]$_.TargetPort -eq $p -and [int]$_.PublishedPort -gt 0 })
    }
    $reachable = $false
    try {
      $probe = [System.Net.Sockets.TcpClient]::new()
      $iar = $probe.BeginConnect("127.0.0.1", $p, $null, $null)
      $reachable = $iar.AsyncWaitHandle.WaitOne(1000, $false)
      if ($reachable) { $probe.EndConnect($iar) }
      $probe.Close()
    } catch {
      $reachable = $false
    }
    if ($published.Count -gt 0) {
      $publishedDesc = (($published | ForEach-Object { "$($_.URL):$($_.PublishedPort)->$($_.TargetPort)/$($_.Protocol)" }) -join ",")
      $results.Add([pscustomobject]@{ Name = "internal.exposure.$svc"; Method = "COMPOSE"; Path = "${svc}:$p"; Status = 1; Expected = "not-published"; Ok = $false; Summary = "compose published $publishedDesc; host_tcp_reachable=$reachable" }) | Out-Null
      $failures.Add("internal service $svc port $p is published by compose: $publishedDesc") | Out-Null
    } else {
      $results.Add([pscustomobject]@{ Name = "internal.exposure.$svc"; Method = "COMPOSE"; Path = "${svc}:$p"; Status = 0; Expected = "not-published"; Ok = $true; Summary = "compose not published; host_tcp_reachable=$reachable" }) | Out-Null
    }
  }

  $top = Invoke-Api "services.topology.summary" GET "/admin/topology" -Token $script:AdminToken -Expected @(200)

  Write-Report "auth-api.md" @("auth.*")
  Write-Report "problem-api.md" @("problem.*")
  Write-Report "judge-user-api.md" @("judge.*")
  Write-Report "worker-api.md" @("worker.*")
  Write-Report "admin-judge-api.md" @("admin.judge.*")
  Write-Report "admin-health-api.md" @("admin.health.*")
  Write-Report "service-registry-api.md" @("services.*")
  Write-Report "internal-exposure-check.md" @("internal.exposure.*")

  if ($top.Json) {
    "`n## Topology Summary`n" | Add-Content (Join-Path $ReportDir "service-registry-api.md")
    "sets=$($top.Json.sets.Count), nodes=$($top.Json.nodes.Count), edges=$($top.Json.edges.Count), components=$($top.Json.components.Count)" | Add-Content (Join-Path $ReportDir "service-registry-api.md")
    "node ids: $((($top.Json.nodes | Select-Object -ExpandProperty service_id) -join ', '))" | Add-Content (Join-Path $ReportDir "service-registry-api.md")
    "component ids: $((($top.Json.components | Select-Object -First 20 -ExpandProperty component_id) -join ', '))" | Add-Content (Join-Path $ReportDir "service-registry-api.md")
  }

  $results | ConvertTo-Json -Depth 20 | Set-Content (Join-Path $ReportDir "runtime-results.json")
  [System.IO.File]::WriteAllText(
    (Join-Path $ReportDir "failures.txt"),
    (($failures | ForEach-Object { [string]$_ }) -join [Environment]::NewLine),
    [System.Text.UTF8Encoding]::new($false)
  )
  [System.IO.File]::WriteAllText(
    (Join-Path $ReportDir "path-leak-findings.txt"),
    (($script:LeakedPaths | ForEach-Object { [string]$_ }) -join [Environment]::NewLine),
    [System.Text.UTF8Encoding]::new($false)
  )

  $summary = [ordered]@{
    total = $results.Count
    failed = $failures.Count
    path_leaks = $script:LeakedPaths.Count
    admin_login_ok = [bool]$script:AdminToken
    user_login_ok = [bool]$script:UserToken
    problem_id = $script:ProblemId
    submission_id = $script:SubmissionId
    worker_submission_id = $script:WorkerSubmissionId
    claimed_task = [bool]$script:ClaimedTask
    admin_health_status = $script:AdminHealthStatus
    admin_health_judge_status = $script:AdminHealthJudgeStatus
  }
  $summary | ConvertTo-Json -Depth 5 | Set-Content (Join-Path $ReportDir "summary.json")
  $summary | ConvertTo-Json -Depth 5

  if ($failures.Count -gt 0 -or $script:LeakedPaths.Count -gt 0) {
    exit 1
  }
} finally {
  if ($script:HttpClient) { $script:HttpClient.Dispose() }
  if ($script:HttpHandler) { $script:HttpHandler.Dispose() }
}
