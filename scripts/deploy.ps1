# PhantomCore Omega — Windows Deployment Script
# Builds everything and deploys to Android device + starts PC daemon

param(
    [string]$DeviceSerial = "",
    [string]$DaemonPort = "42069"
)

$ErrorActionPreference = "Stop"

function Write-Step($step, $msg) {
    Write-Host "`n[$step] $msg" -ForegroundColor Cyan
}

function Write-OK($msg) {
    Write-Host "  ✓ $msg" -ForegroundColor Green
}

function Write-Fail($msg) {
    Write-Host "  ✗ $msg" -ForegroundColor Red
}

$ProjectRoot = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
if (-not (Test-Path "$ProjectRoot\pc_daemon\Cargo.toml")) {
    $ProjectRoot = Split-Path -Parent $PSScriptRoot
}

Write-Host "══════════════════════════════════════════" -ForegroundColor Magenta
Write-Host "  PhantomCore Omega — Deployment Script" -ForegroundColor Magenta
Write-Host "══════════════════════════════════════════" -ForegroundColor Magenta
Write-Host "  Project: $ProjectRoot"

$AdbArgs = @()
if ($DeviceSerial) {
    $AdbArgs = @("-s", $DeviceSerial)
    Write-Host "  Target Device: $DeviceSerial"
}

# ── Step 1: Check Prerequisites ──
Write-Step "1/7" "Checking prerequisites..."

$missing = @()
if (-not (Get-Command "cargo" -ErrorAction SilentlyContinue)) { $missing += "Rust (cargo)" }
if (-not (Get-Command "adb" -ErrorAction SilentlyContinue)) { $missing += "Android SDK (adb)" }

if ($missing.Count -gt 0) {
    Write-Fail "Missing: $($missing -join ', ')"
    Write-Host "  Install Rust from https://rustup.rs"
    Write-Host "  Install Android SDK and add platform-tools to PATH"
    exit 1
}
Write-OK "Rust toolchain found"
Write-OK "ADB found"

# Check ADB device
$devices = adb @AdbArgs devices 2>&1
Write-OK "ADB devices checked"

# ── Step 2: Build PC Daemon ──
Write-Step "2/7" "Building PC daemon (Rust)..."
Push-Location "$ProjectRoot\pc_daemon"
try {
    cargo build --release 2>&1 | ForEach-Object { Write-Host "  $_" -ForegroundColor DarkGray }
    if ($LASTEXITCODE -ne 0) { throw "Cargo build failed" }
    Write-OK "Daemon built: target\release\phantom-core-daemon.exe"
} finally {
    Pop-Location
}

# ── Step 3: Build Proxy APK ──
Write-Step "3/7" "Building Proxy APK..."
$ProxyDir = "$ProjectRoot\android\proxy_app"
if (Test-Path "$ProxyDir\gradlew.bat") {
    Push-Location $ProxyDir
    try {
        & .\gradlew.bat assembleDebug 2>&1 | ForEach-Object { Write-Host "  $_" -ForegroundColor DarkGray }
        Write-OK "Proxy APK built"
    } finally {
        Pop-Location
    }
} else {
    Write-Host "  ⚠ Gradle wrapper not found — run 'gradle wrapper' in $ProxyDir first" -ForegroundColor Yellow
}

# ── Step 4: Build Demo APK ──
Write-Step "4/7" "Building Demo APK..."
$DemoDir = "$ProjectRoot\android\demo_app"
if (Test-Path "$DemoDir\gradlew.bat") {
    Push-Location $DemoDir
    try {
        & .\gradlew.bat assembleDebug 2>&1 | ForEach-Object { Write-Host "  $_" -ForegroundColor DarkGray }
        Write-OK "Demo APK built"
    } finally {
        Pop-Location
    }
} else {
    Write-Host "  ⚠ Gradle wrapper not found — run 'gradle wrapper' in $DemoDir first" -ForegroundColor Yellow
}

# ── Step 5: Install APKs via ADB ──
Write-Step "5/7" "Installing APKs..."
$ProxyApk = Get-ChildItem "$ProxyDir\app\build\outputs\apk\debug\*.apk" -ErrorAction SilentlyContinue | Select-Object -First 1
$DemoApk = Get-ChildItem "$DemoDir\app\build\outputs\apk\debug\*.apk" -ErrorAction SilentlyContinue | Select-Object -First 1

if ($ProxyApk) {
    adb @AdbArgs install -r $ProxyApk.FullName 2>&1
    Write-OK "Proxy APK installed"
} else {
    Write-Host "  ⚠ Proxy APK not found — build first" -ForegroundColor Yellow
}

if ($DemoApk) {
    adb @AdbArgs install -r $DemoApk.FullName 2>&1
    Write-OK "Demo APK installed"
} else {
    Write-Host "  ⚠ Demo APK not found — build first" -ForegroundColor Yellow
}

# ── Step 6: Push eBPF module ──
Write-Step "6/7" "Pushing eBPF module to device..."
$EbpfDir = "$ProjectRoot\android\kernel_module"
adb @AdbArgs push "$EbpfDir\ebpf_program.c" "/data/local/tmp/phantom/" 2>&1
adb @AdbArgs push "$EbpfDir\load.sh" "/data/local/tmp/phantom/" 2>&1
Write-OK "eBPF files pushed to /data/local/tmp/phantom/"
Write-Host "  ⚠ Run 'adb shell su -c sh /data/local/tmp/phantom/load.sh' to load (requires root)" -ForegroundColor Yellow

# ── Step 7: Start PC Daemon ──
Write-Step "7/7" "Starting PC daemon..."

# Get local IP
$LocalIP = (Get-NetIPAddress -AddressFamily IPv4 | Where-Object {
    $_.InterfaceAlias -notmatch "Loopback" -and $_.IPAddress -ne "127.0.0.1"
} | Select-Object -First 1).IPAddress

Write-Host ""
Write-Host "══════════════════════════════════════════" -ForegroundColor Green
Write-Host "  DEPLOYMENT COMPLETE" -ForegroundColor Green
Write-Host "══════════════════════════════════════════" -ForegroundColor Green
Write-Host ""
Write-Host "  PC IP Address: $LocalIP" -ForegroundColor Yellow
Write-Host "  Daemon Port:   $DaemonPort" -ForegroundColor Yellow
Write-Host ""
Write-Host "  Enter this IP in the PhantomCore app on your phone" -ForegroundColor White
Write-Host "  then tap TETHER to start offloading!" -ForegroundColor White
Write-Host ""
Write-Host "  Starting daemon now..." -ForegroundColor Cyan

# Start the daemon
& "$ProjectRoot\pc_daemon\target\release\phantom-core-daemon.exe"
