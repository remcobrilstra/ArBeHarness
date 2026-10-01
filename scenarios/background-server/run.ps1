# Copy seed/ to work/ and run one harness job there.
param(
    [string]$HarnessHome = $(Join-Path $env:TEMP "arbe-scenario-background-server")
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

& $bin --dev-home $HarnessHome --workdir $work --name "background-server" --approve all --print @"
Start python serve.py in the background with the execute tool. Request http://127.0.0.1:8765/ and tell me the response body. Then stop that process with process_kill. Use process_output if you need to see what it printed.
"@
exit $LASTEXITCODE
