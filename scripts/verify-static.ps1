param(
    [switch]$SkipDockerBuild
)

# 用途：执行 OJOS 静态验证，包括 Go、Rust、前端构建、compose config 和安全扫描。
# 运行环境：Windows PowerShell，需安装 go、cargo、npm、docker compose；使用 -SkipDockerBuild 时不需要 Docker daemon 运行镜像构建。
# 执行目录：从仓库根目录执行：powershell -NoProfile -File scripts\verify-static.ps1 [-SkipDockerBuild]
# 依赖工具：go、cargo、npm、docker、rg。
# 失败处理：任一步失败都会抛错并停止；应修复对应模块后重新执行。
$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot

function Step($Name, [scriptblock]$Body) {
    Write-Host ""
    Write-Host "==> $Name" -ForegroundColor Cyan
    & $Body
}

function InDir($Path, [scriptblock]$Body) {
    Push-Location $Path
    try {
        & $Body
    } finally {
        Pop-Location
    }
}

function Run {
    param(
        [string]$Command,
        [Parameter(ValueFromRemainingArguments = $true)]
        [object[]]$CommandArgs
    )

    if ($CommandArgs.Count -eq 1 -and $CommandArgs[0] -is [array]) {
        $CommandArgs = $CommandArgs[0]
    }

    & $Command @CommandArgs
    if ($LASTEXITCODE -ne 0) {
        throw "$Command failed with exit code $LASTEXITCODE"
    }
}

function RunQuiet {
    param(
        [string]$Command,
        [Parameter(ValueFromRemainingArguments = $true)]
        [object[]]$CommandArgs
    )

    if ($CommandArgs.Count -eq 1 -and $CommandArgs[0] -is [array]) {
        $CommandArgs = $CommandArgs[0]
    }

    & $Command @CommandArgs | Out-Null
    if ($LASTEXITCODE -ne 0) {
        throw "$Command failed with exit code $LASTEXITCODE"
    }
}

Step "Go fmt check" {
    $files = Get-ChildItem -Path (Join-Path $root "services") -Recurse -Filter *.go |
        ForEach-Object { $_.FullName }
    if ($files.Count -gt 0) {
        $dirty = gofmt -l $files
        if ($dirty) {
            $dirty | ForEach-Object { Write-Host $_ }
            throw "gofmt check failed"
        }
    }
}

Step "Go build/test shared" {
    InDir (Join-Path $root "services/shared") {
        Run "go" @("build", "./...")
        Run "go" @("test", "./...")
    }
}

foreach ($service in @("auth", "gateway", "problem-api", "judge-api")) {
    Step "Go build/test $service" {
        InDir (Join-Path $root "services/$service") {
            Run "go" @("build", "./...")
            Run "go" @("test", "./...")
        }
    }
}

Step "Rust fmt/check judge-worker" {
    InDir (Join-Path $root "services/judge-worker") {
        Run "cargo" @("fmt", "--check")
        Run "cargo" @("check")
    }
}

Step "Frontend build" {
    InDir (Join-Path $root "frontend") {
        Run "npm" @("run", "build")
    }
}

Step "Docker compose config" {
    InDir $root {
        RunQuiet "docker" @("compose", "--env-file", ".env.example", "-f", "deploy/compose/docker-compose.yml", "config")
        RunQuiet "docker" @("compose", "--env-file", "deploy/worker/.env.example", "-f", "deploy/worker/docker-compose.yml", "config")
    }
}

if (-not $SkipDockerBuild) {
    Step "Docker compose build" {
        InDir $root {
            Run "docker" @("compose", "--env-file", ".env.example", "-f", "deploy/compose/docker-compose.yml", "build")
        }
    }
}

Step "Frontend API direct-call scan" {
    InDir $root {
        $direct = rg -n "fetch\(|axios|http://|https://|mock|Mock|TODO|console\.log" frontend/src
        if ($LASTEXITCODE -eq 0) {
            $allowed = $direct | Where-Object { $_ -notmatch "frontend/src\\api\\client.ts:1:" -and $_ -notmatch "frontend/src\\api\\client.ts:48:" }
            if ($allowed) {
                $allowed | ForEach-Object { Write-Host $_ }
                throw "frontend direct-call/mock scan failed"
            }
        } elseif ($LASTEXITCODE -gt 1) {
            throw "rg failed"
        }
    }
}

Step "Public schema internal-path scan" {
    InDir $root {
        $schemaHits = rg -n "code_path|result_path|stdout_path|stderr_path|checker_log_path|package_dir" `
            frontend/src `
            services/auth/auth.api `
            services/problem-api/problemapi.api `
            services/judge-api/judgeapi.api `
            services/gateway/gateway.api
        if ($LASTEXITCODE -eq 0) {
            $schemaHits | ForEach-Object { Write-Host $_ }
            throw "public schema/internal path scan failed"
        } elseif ($LASTEXITCODE -gt 1) {
            throw "rg failed"
        }
    }
}

Step "Dangerous deployment scan" {
    InDir $root {
        $hits = rg -n "privileged:\s*true|nats://|async_nats|async-nats|NATS_URL|4222" deploy services frontend/src .env.example frontend/.env.example docs `
            --glob "!services/judge-worker/Cargo.lock" `
            --glob "!docs/archive/**"
        if ($LASTEXITCODE -eq 0) {
            $realHits = $hits | Where-Object {
                $_ -notlike "*must not use*privileged: true*" -and
                $_ -notlike "*does not use*privileged: true*" -and
                $_ -notlike "*do not set*privileged: true*"
            }
            if ($realHits) {
                $realHits | ForEach-Object { Write-Host $_ }
                throw "dangerous deployment scan failed"
            }
        } elseif ($LASTEXITCODE -gt 1) {
            throw "rg failed"
        }
    }
}

Write-Host ""
Write-Host "Static verification completed." -ForegroundColor Green
