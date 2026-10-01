# Copy seed/ to work/ and run one harness job there.
# A fresh copy of seed is the way to run the same job again.
# Sessions go to a harness home outside the repo so sign-in files are not committed.
param(
    [string]$HarnessHome = $(Join-Path $env:TEMP "arbe-scenario-fix-greeting")
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

& $bin --dev-home $HarnessHome --workdir $work --name "fix-greeting" --approve all --print @"
test_greet.py fails. Fix greet.py so the test passes. Do not change the test. Run the test to confirm.
"@
exit $LASTEXITCODE
