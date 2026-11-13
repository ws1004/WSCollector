<#
.SYNOPSIS
    릴리스 빌드 + 패키징. Windows PowerShell 5.1+에서 실행.
.DESCRIPTION
    1. Windows GUI, Windows CLI, Linux CLI (x86_64, aarch64) 빌드
    2. Linux GUI는 WSL Ubuntu-22.04에서 빌드
    3. 바이너리를 WSCollector-{GUI|CLI}-<os>-<arch> 이름으로 복사
    4. 플랫폼별 zip + SHA256SUMS.txt 생성
.PARAMETER OutDir
    출력 폴더 (기본: github_release)
.PARAMETER SkipBuild
    빌드 건너뛰고 기존 바이너리로 패키징만
#>
param(
    [string]$OutDir = "github_release",
    [switch]$SkipBuild
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
Set-Location $root

if (-not [System.IO.Path]::IsPathRooted($OutDir)) {
    $OutDir = Join-Path $root $OutDir
}

$staging = Join-Path ([System.IO.Path]::GetTempPath()) "wsc-release-$(Get-Random)"
New-Item -ItemType Directory -Force $staging | Out-Null

function Step($msg) { Write-Host "`n=== $msg ===" -ForegroundColor Cyan }

# --- 빌드 ---
if (-not $SkipBuild) {
    Step "Windows GUI (npm run tauri build)"
    npm run tauri build -- --no-bundle
    if ($LASTEXITCODE -ne 0) { throw "Windows GUI build failed" }

    Step "Windows CLI"
    cargo build -p ws-cli --release
    if ($LASTEXITCODE -ne 0) { throw "Windows CLI build failed" }

    Step "Linux CLI x86_64 (musl)"
    cargo build -p ws-cli --release --target x86_64-unknown-linux-musl
    if ($LASTEXITCODE -ne 0) { throw "Linux x86_64 CLI build failed" }

    Step "Linux CLI aarch64 (musl)"
    cargo build -p ws-cli --release --target aarch64-unknown-linux-musl
    if ($LASTEXITCODE -ne 0) { throw "Linux aarch64 CLI build failed" }

    Step "Linux GUI (WSL Ubuntu-22.04)"
    wsl -d Ubuntu-22.04 -- bash -lc "cd '$($root -replace '\\','/' -replace '^([A-Z]):','/mnt/$1'.ToLower())' && CARGO_TARGET_DIR=/root/wsc-target cargo build --release -p ws-gui --features custom-protocol"
    if ($LASTEXITCODE -ne 0) { throw "Linux GUI build failed" }
}

# --- 바이너리 경로 ---
$bins = @{
    "win-gui"   = "target\release\WSCollector.exe"
    "win-cli"   = "target\release\WSCollector-CLI.exe"
    "linux-cli-x86" = "target\x86_64-unknown-linux-musl\release\WSCollector-CLI"
    "linux-cli-arm" = "target\aarch64-unknown-linux-musl\release\WSCollector-CLI"
}

foreach ($k in $bins.Keys) {
    $p = Join-Path $root $bins[$k]
    if (-not (Test-Path $p)) { throw "Binary not found: $p" }
}

# --- Windows zip ---
Step "WSCollector-windows-x86_64.zip"
$winDir = Join-Path $staging "win"
New-Item -ItemType Directory -Force $winDir | Out-Null
Copy-Item (Join-Path $root $bins["win-gui"]) (Join-Path $winDir "WSCollector-GUI-windows-x86_64.exe")
Copy-Item (Join-Path $root $bins["win-cli"]) (Join-Path $winDir "WSCollector-CLI-windows-x86_64.exe")
$driversDir = Join-Path $winDir "drivers"
New-Item -ItemType Directory -Force $driversDir | Out-Null
Copy-Item (Join-Path $root "drivers\winpmem_x64.sys") $driversDir
Copy-Item (Join-Path $root "drivers\WINPMEM-LICENSE.txt") $driversDir

# --- Linux x86_64 zip ---
Step "WSCollector-linux-x86_64.zip"
$lxDir = Join-Path $staging "linux-x86"
New-Item -ItemType Directory -Force $lxDir | Out-Null
Copy-Item (Join-Path $root $bins["linux-cli-x86"]) (Join-Path $lxDir "WSCollector-CLI-linux-x86_64")
# Linux GUI (WSL 빌드)
$linuxGui = "\\wsl$\Ubuntu-22.04\root\wsc-target\release\WSCollector"
if (Test-Path $linuxGui) {
    Copy-Item $linuxGui (Join-Path $lxDir "WSCollector-GUI-linux-x86_64")
} else {
    Write-Warning "Linux GUI not found at $linuxGui - skipping"
}

# --- Linux aarch64 zip ---
Step "WSCollector-linux-aarch64.zip"
$laDir = Join-Path $staging "linux-arm"
New-Item -ItemType Directory -Force $laDir | Out-Null
Copy-Item (Join-Path $root $bins["linux-cli-arm"]) (Join-Path $laDir "WSCollector-CLI-linux-aarch64")

# --- zip 생성 ---
Step "Creating zips"
New-Item -ItemType Directory -Force $OutDir | Out-Null
$zips = @(
    @{ Name = "WSCollector-windows-x86_64.zip"; Source = "$winDir\*" }
    @{ Name = "WSCollector-linux-x86_64.zip";   Source = "$lxDir\*" }
    @{ Name = "WSCollector-linux-aarch64.zip";   Source = "$laDir\*" }
)
foreach ($z in $zips) {
    $dest = Join-Path $OutDir $z.Name
    Remove-Item $dest -Force -ErrorAction SilentlyContinue
    Compress-Archive -Path $z.Source -DestinationPath $dest
    Write-Host "  $($z.Name) ($('{0:N0}' -f (Get-Item $dest).Length) bytes)"
}

# --- SHA256SUMS ---
Step "SHA256SUMS.txt"
$hashFile = Join-Path $OutDir "SHA256SUMS.txt"
$lines = @()
foreach ($z in $zips) {
    $hash = (Get-FileHash (Join-Path $OutDir $z.Name) -Algorithm SHA256).Hash.ToLower()
    $lines += "$hash *$($z.Name)"
}
[System.IO.File]::WriteAllText($hashFile, ($lines -join "`n") + "`n", [System.Text.UTF8Encoding]::new($false))
Get-Content $hashFile

# --- 정리 ---
Remove-Item $staging -Recurse -Force -ErrorAction SilentlyContinue

Step "Done"
Write-Host "Output: $OutDir"
Get-ChildItem $OutDir | Format-Table Name, @{L="Size";E={'{0:N0}' -f $_.Length}} -AutoSize
