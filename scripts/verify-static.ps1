param(
    [switch]$SkipDockerBuild
)

# 用途：执行 OJOS 静态验证，包括 Go、Rust、前端构建、compose config 和安全扫描。
# 运行环境：Windows PowerShell，需安装 go、cargo、npm、docker compose；使用 -SkipDockerBuild 时不需要 Docker daemon 运行镜像构建。
# 执行目录：从仓库根目录执行：powershell -NoProfile -File scripts\verify-static.ps1 [-SkipDockerBuild]
# 依赖工具：go、cargo、npm、docker；推荐安装 rg，脚本会在 rg 不可执行时回退到 Select-String。
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

function Test-RgAvailable {
    if ($null -ne $script:RgAvailable) {
        return $script:RgAvailable
    }

    try {
        $cmd = Get-Command "rg" -ErrorAction SilentlyContinue
        if (-not $cmd) {
            $script:RgAvailable = $false
            return $false
        }

        & rg --version | Out-Null
        $script:RgAvailable = ($LASTEXITCODE -eq 0)
        return $script:RgAvailable
    } catch {
        $script:RgAvailable = $false
        return $false
    }
}

function Get-SearchFiles {
    param(
        [string[]]$Paths,
        [string[]]$ExcludeGlob = @()
    )

    $exclude = $ExcludeGlob | ForEach-Object { $_.TrimStart("!") }
    $files = @()

    foreach ($path in $Paths) {
        if (-not (Test-Path -LiteralPath $path)) {
            continue
        }

        $item = Get-Item -LiteralPath $path
        if ($item.PSIsContainer) {
            $files += Get-ChildItem -LiteralPath $item.FullName -Recurse -File
        } else {
            $files += $item
        }
    }

    $files | Where-Object {
        if ($_.FullName -match "\\(target|node_modules|dist)\\") {
            return $false
        }
        if (@(".exe", ".dll", ".so", ".dylib", ".test", ".out") -contains $_.Extension) {
            return $false
        }

        $rel = $_.FullName.Substring($root.Length + 1).Replace("\", "/")
        foreach ($pattern in $exclude) {
            if ($pattern.EndsWith("/**") -and $rel.StartsWith($pattern.Substring(0, $pattern.Length - 3))) {
                return $false
            }
            if ($rel -like $pattern) {
                return $false
            }
        }
        return $true
    }
}

function Search-Text {
    param(
        [string]$Pattern,
        [string[]]$Paths,
        [string[]]$ExcludeGlob = @()
    )

    if (Test-RgAvailable) {
        $args = @("-n", $Pattern)
        foreach ($glob in $ExcludeGlob) {
            $args += @("--glob", $glob)
        }
        $args += $Paths

        $hits = & rg @args
        if ($LASTEXITCODE -eq 0) {
            return @($hits)
        }
        if ($LASTEXITCODE -eq 1) {
            return @()
        }
        throw "rg failed"
    }

    $files = @(Get-SearchFiles -Paths $Paths -ExcludeGlob $ExcludeGlob)
    if ($files.Count -eq 0) {
        return @()
    }

    $hits = Select-String -Path ($files | ForEach-Object { $_.FullName }) -Pattern $Pattern
    return @($hits | ForEach-Object {
        $rel = $_.Path.Substring($root.Length + 1).Replace("\", "/")
        "{0}:{1}:{2}" -f $rel, $_.LineNumber, $_.Line
    })
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

Step "Rust fmt/check module-installer workspace" {
    InDir $root {
        Run "cargo" @("fmt", "--check")
        Run "cargo" @("check")
        Run "cargo" @("test")
        Run "cargo" @("run", "-p", "ojosctl", "--", "--version")
        Run "cargo" @("run", "-p", "ojosctl", "--", "module", "doctor")
        Run "cargo" @("run", "-p", "ojosctl", "--", "module", "validate", "modules/demo-module/module.yaml", "--repo-root", ".")
        Run "cargo" @("run", "-p", "ojosctl", "--", "module", "package", "modules/demo-module", "-o", ".tmp/agent/scratch/verify-static-demo.ojosmod")
        Run "cargo" @("run", "-p", "ojosctl", "--", "module", "verify", ".tmp/agent/scratch/verify-static-demo.ojosmod")
        Run "cargo" @("run", "-p", "ojosctl", "--", "module", "inspect", ".tmp/agent/scratch/verify-static-demo.ojosmod")
        Run "cargo" @("run", "-p", "ojosctl", "--", "runtime", "services")
        Run "cargo" @("run", "-p", "ojosctl", "--", "runtime", "service", "problem-api")
        Run "cargo" @("run", "-p", "ojosctl", "--", "runtime", "plan-restart", "problem-api")
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
        if (-not (Test-Path (Join-Path $root "kernel/installer/service/Dockerfile"))) {
            throw "module-installer Dockerfile is missing"
        }
        $installerDockerfile = Get-Content (Join-Path $root "kernel/installer/service/Dockerfile") -Raw
        $installerFrom = @($installerDockerfile -split "`n" | Where-Object { $_ -match "^\s*FROM\s+" })
        if ($installerFrom.Count -lt 2) {
            throw "module-installer Dockerfile must use a builder and runtime stage"
        }
        if ($installerFrom[-1] -match "rust:") {
            throw "module-installer final runtime image must not be rust:*"
        }
        $compose = Get-Content (Join-Path $root "deploy/compose/docker-compose.yml") -Raw
        foreach ($required in @("module-installer:", "read_only: true", "no-new-privileges:true", "cap_drop:", "expose:", "../../modules:/workspace/modules:ro", "MODULE_INSTALLER_LOCK_TTL_SECONDS")) {
            if ($compose -notlike "*$required*") {
                throw "compose module-installer hardening check failed: missing $required"
            }
        }
    }
}

if (-not $SkipDockerBuild) {
    Step "Docker compose build" {
        InDir $root {
            Run "docker" @("compose", "--env-file", ".env.example", "-f", "deploy/compose/docker-compose.yml", "build", "module-installer")
            Run "docker" @("compose", "--env-file", ".env.example", "-f", "deploy/compose/docker-compose.yml", "build")
        }
    }
}

Step "Frontend API direct-call scan" {
    InDir $root {
        $direct = Search-Text "fetch\(|axios|http://|https://|mock|Mock|TODO|console\.log" @("frontend/src")
        if ($direct.Count -gt 0) {
            $allowed = $direct | Where-Object { $_ -notmatch "frontend[/\\]src[/\\]api[/\\]client.ts:" }
            if ($allowed) {
                $allowed | ForEach-Object { Write-Host $_ }
                throw "frontend direct-call/mock scan failed"
            }
        }
    }
}

Step "Public schema internal-path scan" {
    InDir $root {
        $schemaHits = Search-Text "code_path|result_path|stdout_path|stderr_path|checker_log_path|package_dir" @(
            "frontend/src",
            "services/auth/auth.api",
            "services/problem-api/problemapi.api",
            "services/judge-api/judgeapi.api",
            "services/gateway/gateway.api"
        )
        if ($schemaHits.Count -gt 0) {
            $schemaHits | ForEach-Object { Write-Host $_ }
            throw "public schema/internal path scan failed"
        }
    }
}

Step "Dangerous deployment scan" {
    InDir $root {
        $hits = Search-Text "privileged:\s*true|nats://|async_nats|async-nats|NATS_URL|4222" @(
            "deploy",
            "services",
            "frontend/src",
            ".env.example",
            "frontend/.env.example",
            "docs"
        ) @("!services/judge-worker/Cargo.lock", "!docs/archive/**")
        if ($hits.Count -gt 0) {
            $realHits = $hits | Where-Object {
                $_ -notlike "*must not use*privileged: true*" -and
                $_ -notlike "*does not use*privileged: true*" -and
                $_ -notlike "*do not set*privileged: true*"
            }
            if ($realHits) {
                $realHits | ForEach-Object { Write-Host $_ }
                throw "dangerous deployment scan failed"
            }
        }
    }
}

Write-Host ""
Write-Host "Static verification completed." -ForegroundColor Green
