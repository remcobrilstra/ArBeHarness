# Three turns in one session. Turn 3 must recall the passphrase from turn 1
# without reading the file. A small context budget and compact_summary make
# a long second turn able to push the first turn into a summary.
param(
    [string]$HarnessHome = $(Join-Path $env:TEMP "arbe-scenario-remember")
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $MyInvocation.MyCommand.Path
$repo = Resolve-Path (Join-Path $root "..\..")
$seed = Join-Path $root "seed"
$work = Join-Path $root "work"
$bin = Join-Path $repo "target\debug\arbeharness.exe"
$userHome = Join-Path $env:USERPROFILE ".arbe"
$config = Join-Path $root "context.toml"

if (-not (Test-Path $bin)) {
    throw "Build the harness first: cargo build -p arbeharness --bin arbeharness"
}

if (Test-Path $work) { Remove-Item $work -Recurse -Force }
Copy-Item $seed $work -Recurse
$filler = Join-Path $work "filler.txt"
$lines = 1..400 | ForEach-Object { "filler line {0:d4} {1}" -f $_, ("x" * 60) }
[System.IO.File]::WriteAllLines($filler, $lines)

New-Item -ItemType Directory -Force -Path (Join-Path $HarnessHome "auth") | Out-Null
New-Item -ItemType Directory -Force -Path (Join-Path $HarnessHome "config") | Out-Null
Copy-Item (Join-Path $userHome "config\config.toml") (Join-Path $HarnessHome "config\config.toml") -Force
Copy-Item (Join-Path $userHome "auth\*") (Join-Path $HarnessHome "auth") -Force

function Invoke-Turn([string[]]$Extra) {
    & $bin --dev-home $HarnessHome --workdir $work --config $config @Extra
    if ($LASTEXITCODE -ne 0) { throw "harness exited $LASTEXITCODE" }
}

Write-Output "work=$work"
Write-Output "home=$HarnessHome"

Invoke-Turn @(
    "--name", "remember",
    "--approve", "all",
    "--print", "The passphrase for this exercise is violet-319. Write that exact string to secret.txt and do nothing else."
)

$match = Get-ChildItem (Join-Path $HarnessHome "sessions") -Directory | ForEach-Object {
    $metaPath = Join-Path $_.FullName "meta.json"
    if (-not (Test-Path $metaPath)) { return }
    $meta = Get-Content $metaPath -Raw | ConvertFrom-Json
    if ($meta.title -eq "remember") { $meta }
} | Sort-Object updated_at -Descending | Select-Object -First 1
if (-not $match) { throw "no remember session" }
Write-Output "session=$($match.id)"

Invoke-Turn @(
    "--resume", $match.id,
    "--approve", "all",
    "--print", "Read filler.txt and reply with its line count only."
)
Invoke-Turn @(
    "--resume", $match.id,
    "--approve", "all",
    "--print", "What was the passphrase from the first message? Reply with the passphrase only. Do not read any file."
)
exit 0
