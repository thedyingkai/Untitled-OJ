param(
    [switch]$SkipDockerBuild,
    [switch]$SkipFrontend,
    [switch]$SkipRust,
    [switch]$SkipGo
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot

function Step($Name, [scriptblock]$Body) {
    Write-Host ""
    Write-Host "==> $Name" -ForegroundColor Cyan
    & $Body
}

function InDir($Path, [scriptblock]$Body) {
    Push-Location $Path
    try { & $Body } finally { Pop-Location }
}

function Run {
    param([string]$Command, [Parameter(ValueFromRemainingArguments = $true)][object[]]$CommandArgs)
    if ($CommandArgs.Count -eq 1 -and $CommandArgs[0] -is [array]) { $CommandArgs = $CommandArgs[0] }
    & $Command @CommandArgs
    if ($LASTEXITCODE -ne 0) { throw "$Command failed with exit code $LASTEXITCODE" }
}

function RunQuiet {
    param([string]$Command, [Parameter(ValueFromRemainingArguments = $true)][object[]]$CommandArgs)
    if ($CommandArgs.Count -eq 1 -and $CommandArgs[0] -is [array]) { $CommandArgs = $CommandArgs[0] }
    & $Command @CommandArgs | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "$Command failed with exit code $LASTEXITCODE" }
}

if (-not $SkipGo) {
    Step "Go fmt check" {
        $files = Get-ChildItem -Path (Join-Path $root "services") -Recurse -Filter *.go | ForEach-Object { $_.FullName }
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
    Step "Rust service-first workspace" {
        InDir $root {
            Run "cargo" @("fmt", "--check")
            Run "cargo" @("check", "--workspace", "--all-targets")
            Run "cargo" @("test", "--workspace")
            Run "cargo" @("run", "-p", "ojosctl", "--", "--version")
            Run "cargo" @("run", "-p", "ojos-installer-tui", "--", "--version")
            Run "cargo" @("run", "-p", "ojosctl", "--", "--json", "doctor")
            Run "cargo" @("run", "-p", "ojosctl", "--", "--json", "status")
            Run "cargo" @("run", "-p", "ojosctl", "--", "service", "discover")
            Run "cargo" @("run", "-p", "ojosctl", "--", "service", "validate", "services/gateway/service.yaml")
            Run "cargo" @("run", "-p", "ojosctl", "--", "service", "validate", "services/web-shell/service.yaml")
            Run "cargo" @("run", "-p", "ojosctl", "--", "service", "validate", "services/problem-api/service.yaml")
            Run "cargo" @("run", "-p", "ojosctl", "--", "service", "validate", "services/judge-api/service.yaml")
            Run "cargo" @("run", "-p", "ojosctl", "--", "service", "validate", "services/judge-worker/service.yaml")
            Run "cargo" @("run", "-p", "ojosctl", "--", "service", "install-plan", "services/judge-worker/service.yaml")
            Run "cargo" @("run", "-p", "ojosctl", "--", "service", "package", "services/gateway", "-o", ".tmp/release/gateway.ojos-service")
            Run "cargo" @("run", "-p", "ojosctl", "--", "service", "verify", ".tmp/release/gateway.ojos-service")
            Run "cargo" @("run", "-p", "ojosctl", "--", "set", "list")
            Run "cargo" @("run", "-p", "ojosctl", "--", "set", "expand", "sets/single-node-oj.yaml")
            Run "cargo" @("run", "-p", "ojosctl", "--", "endpoint", "validate", "192.168.1.10:8082")
            Run "cargo" @("run", "-p", "ojosctl", "--", "link", "plan-create", "192.168.1.21:9101", "192.168.1.10:8082")
            Run "cargo" @("run", "-p", "ojosctl", "--", "topology", "snapshot")
            Run "cargo" @("run", "-p", "ojosctl", "--", "runtime", "services")
        }
    }

    Step "Rust judge-worker" {
        InDir (Join-Path $root "services/judge-worker") {
            Run "cargo" @("fmt", "--check")
            Run "cargo" @("check")
            Run "cargo" @("test")
        }
    }
}

if (-not $SkipFrontend) {
    Step "Frontend build" {
        InDir (Join-Path $root "frontend") {
            Run "npm" @("audit", "--registry=https://registry.npmjs.org", "--audit-level=high")
            Run "npm" @("run", "build")
        }
    }
}

Step "Docker compose config" {
    InDir $root {
        RunQuiet "docker" @("compose", "--env-file", ".env.example", "-f", "deploy/compose/docker-compose.yml", "config")
        RunQuiet "docker" @("compose", "--env-file", "deploy/worker/.env.example", "-f", "deploy/worker/docker-compose.yml", "config")
        $compose = Get-Content (Join-Path $root "deploy/compose/docker-compose.yml") -Raw
        foreach ($required in @("root-runtime-manager:", "read_only: true", "no-new-privileges:true", "cap_drop:", "expose:", "../../services:/workspace/services:ro", "../../sets:/workspace/sets:ro", "MODULE_INSTALLER_LOCK_TTL_SECONDS")) {
            if ($compose -notlike "*$required*") {
                throw "compose root runtime manager hardening check failed: missing $required"
            }
        }
    }
}

if (-not $SkipDockerBuild) {
    Step "Docker compose build" {
        InDir $root {
            Run "docker" @("compose", "--env-file", ".env.example", "-f", "deploy/compose/docker-compose.yml", "build", "root-runtime-manager")
            Run "docker" @("compose", "--env-file", ".env.example", "-f", "deploy/compose/docker-compose.yml", "build")
        }
    }
}

Write-Host ""
Write-Host "Static verification completed." -ForegroundColor Green
