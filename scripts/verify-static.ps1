param(
    [switch]$SkipDockerBuild,
    [switch]$SkipFrontend,
    [switch]$SkipRust,
    [switch]$SkipGo
)

# 用途：执行 OJOS 静态验收，覆盖 Go、Rust、前端构建、compose config、Installer CLI/TUI smoke 和模块包校验。
# 运行环境：Windows PowerShell，需要 go、cargo、npm、docker compose；使用 -SkipDockerBuild 时不要求构建镜像。
# 执行目录：仓库根目录，例如 powershell -NoProfile -File scripts\verify-static.ps1 -SkipDockerBuild。
# 失败处理：任一步失败都会抛错并停止；修复对应模块后重新执行。
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

if (-not $SkipGo) {
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
}

if (-not $SkipRust) {
Step "Rust fmt/check module-installer workspace" {
    InDir $root {
        Run "cargo" @("fmt", "--check")
        Run "cargo" @("check", "--workspace", "--all-targets")
        Run "cargo" @("test", "--workspace")
        Run "cargo" @("clippy", "--workspace", "--all-targets", "--", "-D", "warnings")
        Run "cargo" @("run", "-p", "ojosctl", "--", "--version")
        Run "cargo" @("run", "-p", "ojos-installer-tui", "--", "--version")
        Run "cargo" @("run", "-p", "ojosctl", "--", "--json", "doctor")
        Run "cargo" @("run", "-p", "ojosctl", "--", "--json", "status")
        Run "cargo" @("run", "-p", "ojosctl", "--", "module", "doctor")
        if (Test-Path ".tmp/agent/scratch/verify-static-sample-init") {
            Remove-Item -Recurse -Force ".tmp/agent/scratch/verify-static-sample-init"
        }
        Run "cargo" @("run", "-p", "ojosctl", "--", "module", "init", "ojos.verify-static-sample", "--name", "Verify Static Sample", "--kind", "feature", "--out", ".tmp/agent/scratch/verify-static-sample-init", "--with-topology")
        Run "cargo" @("run", "-p", "ojosctl", "--", "module", "validate", "modules/demo-module/module.yaml", "--repo-root", ".")
        Run "cargo" @("run", "-p", "ojosctl", "--", "module", "validate", "modules/sample-hello/module.yaml", "--repo-root", ".")
        Run "cargo" @("run", "-p", "ojosctl", "--", "module", "install-plan", "modules/sample-hello/module.yaml", "--repo-root", ".")
        Run "cargo" @("run", "-p", "ojosctl", "--", "module", "install", "modules/sample-hello/module.yaml", "--dry-run", "--repo-root", ".")
        Run "cargo" @("run", "-p", "ojosctl", "--", "module", "enable", "ojos.sample-hello", "--repo-root", ".")
        Run "cargo" @("run", "-p", "ojosctl", "--", "module", "disable", "ojos.sample-hello", "--repo-root", ".")
        Run "cargo" @("run", "-p", "ojosctl", "--", "module", "uninstall-dry-run", "ojos.sample-hello", "--repo-root", ".")
        Run "cargo" @("run", "-p", "ojosctl", "--", "module", "package", "modules/demo-module", "-o", ".tmp/agent/scratch/verify-static-demo.ojosmod")
        Run "cargo" @("run", "-p", "ojosctl", "--", "module", "verify", ".tmp/agent/scratch/verify-static-demo.ojosmod")
        Run "cargo" @("run", "-p", "ojosctl", "--", "module", "inspect", ".tmp/agent/scratch/verify-static-demo.ojosmod")
        Run "cargo" @("run", "-p", "ojosctl", "--", "module", "package", "modules/sample-hello", "-o", ".tmp/agent/scratch/verify-static-sample-hello.ojosmod")
        Run "cargo" @("run", "-p", "ojosctl", "--", "module", "verify", ".tmp/agent/scratch/verify-static-sample-hello.ojosmod")
        Run "cargo" @("run", "-p", "ojosctl", "--", "runtime", "snapshot")
        Run "cargo" @("run", "-p", "ojosctl", "--", "runtime", "routes")
        Run "cargo" @("run", "-p", "ojosctl", "--", "runtime", "services")
        Run "cargo" @("run", "-p", "ojosctl", "--", "runtime", "service", "problem-api")
        Run "cargo" @("run", "-p", "ojosctl", "--", "runtime", "plan-restart", "problem-api", "--out", ".tmp/agent/scratch/problem-api-restart.json")
        Run "cargo" @("run", "-p", "ojosctl", "--", "runtime", "apply-plan", ".tmp/agent/scratch/problem-api-restart.json", "--dry-run")
        Run "cargo" @("run", "-p", "ojosctl", "--", "runtime", "operations")
    }
}

Step "Rust fmt/check judge-worker" {
    InDir (Join-Path $root "services/judge-worker") {
        Run "cargo" @("fmt", "--check")
        Run "cargo" @("check")
    }
}
}

if (-not $SkipFrontend) {
Step "Frontend build" {
    InDir (Join-Path $root "frontend") {
        Run "npm" @("run", "build")
    }
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

Write-Host ""
Write-Host "Static verification completed." -ForegroundColor Green
