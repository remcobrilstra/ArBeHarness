param(
    [string]$HarnessHome = $(Join-Path $env:TEMP "arbe-scenario-long-output"),
    [string]$SessionId
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $MyInvocation.MyCommand.Path
$repo = (Resolve-Path (Join-Path $root "..\..")).Path
$bin = Join-Path $repo "target\debug\arbeharness.exe"

if (-not $SessionId) {
    $sessions = Join-Path $HarnessHome "sessions"
    $match = Get-ChildItem $sessions -Directory | ForEach-Object {
        $metaPath = Join-Path $_.FullName "meta.json"
        if (-not (Test-Path $metaPath)) { return }
        $meta = Get-Content $metaPath -Raw | ConvertFrom-Json
        if ($meta.title -eq "long-output") { $meta }
    } | Sort-Object updated_at -Descending | Select-Object -First 1
    if (-not $match) { throw "No long-output session under $sessions" }
    $SessionId = $match.id
}

Write-Output "session=$SessionId"
Write-Output "home=$HarnessHome"

& $bin --dev-home $HarnessHome --workdir $repo --name "session-review" --approve all --print @"
Follow the session-review skill exactly for session $SessionId. The harness home for that session is $HarnessHome. Write the finished report to scenarios/long-output/review.md and print the same report as your answer. Do not change the session files.
"@
exit $LASTEXITCODE
