<#
.SYNOPSIS
  Download a pinned, checksum-verified FFmpeg build (ffmpeg.exe + ffprobe.exe).

.DESCRIPTION
  ClipLite shells out to FFmpeg for capture (ddagrab), encoding (NVENC/AMF/QSV/
  libx264) and clip extraction. Instead of committing a 100+ MB binary, this
  script fetches an exact release and verifies its SHA-256 so every build uses
  identical bits.

  Pinned build: gyan.dev "essentials" 8.1.1 (GPL, includes libx264).
  Source:       https://github.com/GyanD/codexffmpeg/releases/tag/8.1.1

  Why not newer: gyan.dev builds from 8.1.2 / 9.0.x onward link NVENC API 13.1,
  which requires NVIDIA driver >= 610 and fails to open h264_nvenc on older
  drivers ("Required: 13.1 Found: 13.0"). 8.1.1 uses NVENC API 13.0 and works
  on the 59x drivers most users have. Re-check before bumping.
  Licensing:    see docs (WS5) — GPL build because libx264 is the CPU fallback.

.PARAMETER Dest
  Directory to place ffmpeg.exe / ffprobe.exe in.
  Default: apps/desktop/src-tauri/bin (relative to the repo root).

.EXAMPLE
  powershell -File scripts/fetch-ffmpeg.ps1
  powershell -File scripts/fetch-ffmpeg.ps1 -Dest "..\other checkout\src-tauri\bin"
#>
param(
    [string]$Dest = ""
)

$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"

$Version = "8.1.1"
$Url     = "https://github.com/GyanD/codexffmpeg/releases/download/$Version/ffmpeg-$Version-essentials_build.zip"
$Sha256  = "6F58CE889F59C311410F7D2B18895B33C03456463486F3B1EBC93D97A0F54541"
$Stamp   = "ffmpeg-$Version-essentials.sha256"

$RepoRoot = Split-Path -Parent $PSScriptRoot
if ([string]::IsNullOrWhiteSpace($Dest)) {
    $Dest = Join-Path $RepoRoot "apps\desktop\src-tauri\bin"
}
New-Item -ItemType Directory -Force -Path $Dest | Out-Null
$Dest = (Resolve-Path -LiteralPath $Dest).Path

$StampPath = Join-Path $Dest $Stamp
$Ffmpeg    = Join-Path $Dest "ffmpeg.exe"
$Ffprobe   = Join-Path $Dest "ffprobe.exe"

# Idempotent: skip when this exact archive was already installed here.
if ((Test-Path -LiteralPath $StampPath) -and (Test-Path -LiteralPath $Ffmpeg) -and (Test-Path -LiteralPath $Ffprobe)) {
    if ((Get-Content -LiteralPath $StampPath -Raw).Trim() -eq $Sha256) {
        Write-Host "FFmpeg $Version already present in '$Dest'."
        exit 0
    }
}

$Work = Join-Path ([System.IO.Path]::GetTempPath()) ("cliplite-ffmpeg-" + [guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Force -Path $Work | Out-Null
try {
    $Zip = Join-Path $Work "ffmpeg.zip"
    Write-Host "Downloading FFmpeg $Version ..."
    Invoke-WebRequest -Uri $Url -OutFile $Zip

    $Actual = (Get-FileHash -LiteralPath $Zip -Algorithm SHA256).Hash
    if ($Actual -ne $Sha256) {
        throw "SHA-256 mismatch for $Url`n  expected $Sha256`n  actual   $Actual"
    }

    Expand-Archive -LiteralPath $Zip -DestinationPath (Join-Path $Work "x") -Force
    $Bin = Get-ChildItem -LiteralPath (Join-Path $Work "x") -Recurse -Directory -Filter "bin" | Select-Object -First 1
    if (-not $Bin) { throw "Archive layout unexpected: no bin/ directory" }

    Copy-Item -LiteralPath (Join-Path $Bin.FullName "ffmpeg.exe")  -Destination $Ffmpeg  -Force
    Copy-Item -LiteralPath (Join-Path $Bin.FullName "ffprobe.exe") -Destination $Ffprobe -Force
    # Copy-Item keeps the archive's (old) timestamps; Cargo's change detection
    # is mtime-based, so stamp "now" or tauri-build won't re-copy bin/ next to
    # the exe and a stale FFmpeg keeps being used.
    $Now = Get-Date
    (Get-Item -LiteralPath $Ffmpeg).LastWriteTime  = $Now
    (Get-Item -LiteralPath $Ffprobe).LastWriteTime = $Now
    Get-ChildItem -LiteralPath $Dest -Filter "ffmpeg-*.sha256" | Remove-Item -Force
    Set-Content -LiteralPath $StampPath -Value $Sha256 -Encoding ascii
    Write-Host "Installed FFmpeg $Version into '$Dest'."
}
finally {
    Remove-Item -LiteralPath $Work -Recurse -Force -ErrorAction SilentlyContinue
}
