# One plan-mode turn. Edits are refused until exit_plan_mode is approved.
param(
    [string]$HarnessHome = $(Join-Path $env:TEMP "arbe-scenario-plan-mode")
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $MyInvocation.MyCommand.Path
$repo = Resolve-Path (Join-Path $root "..\..")
$seed = Join-Path $root "seed"
$work = Join-Path $root "work"
$bin = Join-Path $repo "target\debug\arbeharness.exe"
$userHome = Join-Path $env:USERPROFILE ".arbe"

if (-not (Test-Path $bin)) {
    throw "Build the harness first: cargo build -p arbeharness --bin arbeharness"
}

if (Test-Path $work) { Remove-Item $work -Recurse -Force }
Copy-Item $seed $work -Recurse

New-Item -ItemType Directory -Force -Path (Join-Path $HarnessHome "auth") | Out-Null
New-Item -ItemType Directory -Force -Path (Join-Path $HarnessHome "config") | Out-Null
Copy-Item (Join-Path $userHome "config\config.toml") (Join-Path $HarnessHome "config\config.toml") -Force
Copy-Item (Join-Path $userHome "auth\*") (Join-Path $HarnessHome "auth") -Force

Write-Output "work=$work"
Write-Output "home=$HarnessHome"

& $bin --dev-home $HarnessHome --workdir $work --name "plan-mode" --mode plan --approve all --print @"
test_marker.py fails. Investigate and produce a plan, then carry it out once the plan is approved. Do not change the test.
"@
exit $LASTEXITCODE
