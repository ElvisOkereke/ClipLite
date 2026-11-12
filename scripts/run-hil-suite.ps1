<#
.SYNOPSIS
  Run the full hardware-in-the-loop suite against one ClipLite build, unattended.

.DESCRIPTION
  Order: elevated hotkey matrix first (needs one UAC approval, so it runs while
  you are still at the PC), then the normal matrix, the benchmark and the soak.
  Each step writes into its own folder under -Out; a transcript is kept in
  -Out\suite.log. Steps that fail are recorded and the suite continues.

  Takes over the screen (fake game, synthetic key presses) for the whole run.
  Close games and stop your own ClipLite's replay buffer before starting.

.PARAMETER App
  Path to the cliplite.exe build under test.
.PARAMETER Out
  Results folder (e.g. docs\benchmarks\raw\baseline).
.PARAMETER Trials
  Trials per matrix cell (default 5).
.PARAMETER SoakHours
  Soak length in hours (default 2).
.PARAMETER SkipElevated
  Skip the elevated matrix (no UAC prompt).

.EXAMPLE
  powershell -File scripts\run-hil-suite.ps1 -App ..\cliplite-baseline\src-tauri\target\release\cliplite.exe -Out docs\benchmarks\raw\baseline
#>
param(
    [Parameter(Mandatory = $true)][string]$App,
    [Parameter(Mandatory = $true)][string]$Out,
    [int]$Trials = 5,
    [double]$SoakHours = 2,
    [switch]$SkipElevated
)

$ErrorActionPreference = "Continue"
$RepoRoot = Split-Path -Parent $PSScriptRoot
$Hil = Join-Path $RepoRoot "target\release\hil.exe"
if (-not (Test-Path -LiteralPath $Hil)) {
    Write-Host "Building the test kit (release)..."
    Push-Location $RepoRoot; cargo build --release -p cliplite-testkit; Pop-Location
}
if (-not (Test-Path -LiteralPath $App)) { throw "App not found: $App" }
New-Item -ItemType Directory -Force -Path $Out | Out-Null
$Out = (Resolve-Path -LiteralPath $Out).Path
$App = (Resolve-Path -LiteralPath $App).Path
Start-Transcript -Path (Join-Path $Out "suite.log") -Append | Out-Null

$results = [ordered]@{}
function Step([string]$Name, [string[]]$HilArgs) {
    Write-Host "`n=== $Name  ($(Get-Date -Format 'HH:mm:ss')) ===" -ForegroundColor Cyan
    # Windows PowerShell's transcript does not capture native output; echo it.
    & $Hil @HilArgs 2>&1 | ForEach-Object { Write-Host $_ }
    $results[$Name] = if ($LASTEXITCODE -eq 0) { "ok" } else { "FAILED (exit $LASTEXITCODE)" }
}

if (-not $SkipElevated) {
    Write-Host "Approve the UAC prompt for the elevated helper..." -ForegroundColor Yellow
    Step "matrix-elevated" @("matrix", "--app", $App, "--trials", "$Trials", "--elevated", "--out", (Join-Path $Out "matrix-elevated"))
}
Step "matrix" @("matrix", "--app", $App, "--trials", "$Trials", "--out", (Join-Path $Out "matrix"))
Step "bench"  @("bench",  "--app", $App, "--runs", "5", "--duration-secs", "60", "--out", (Join-Path $Out "bench"))
Step "soak"   @("soak",   "--app", $App, "--hours", "$SoakHours", "--out", (Join-Path $Out "soak"))

Write-Host "`n=== Suite finished $(Get-Date -Format 'HH:mm:ss') ===" -ForegroundColor Cyan
$results.GetEnumerator() | ForEach-Object { Write-Host ("{0,-16} {1}" -f $_.Key, $_.Value) }
Stop-Transcript | Out-Null
