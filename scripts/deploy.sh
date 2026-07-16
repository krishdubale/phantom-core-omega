#!/usr/bin/env bash
# PhantomCore Omega — Linux/macOS Deployment Script
# Builds everything and deploys to Android device + starts PC daemon

set -euo pipefail

DEVICE_SERIAL="${1:-}"
DAEMON_PORT="42069"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(dirname "$SCRIPT_DIR")"

C_RESET="\033[0m"
C_CYAN="\033[36m"
C_GREEN="\033[32m"
C_RED="\033[31m"
C_YELLOW="\033[33m"
C_MAGENTA="\033[35m"

step() { echo -e "\n${C_CYAN}[$1] $2${C_RESET}"; }
ok()   { echo -e "  ${C_GREEN}✓ $1${C_RESET}"; }
fail() { echo -e "  ${C_RED}✗ $1${C_RESET}"; }
warn() { echo -e "  ${C_YELLOW}⚠ $1${C_RESET}"; }

ADB_ARGS=""
[ -n "$DEVICE_SERIAL" ] && ADB_ARGS="-s $DEVICE_SERIAL"

echo -e "${C_MAGENTA}══════════════════════════════════════════${C_RESET}"
echo -e "${C_MAGENTA}  PhantomCore Omega — Deployment Script${C_RESET}"
echo -e "${C_MAGENTA}══════════════════════════════════════════${C_RESET}"
echo "  Project: $PROJECT_ROOT"

# ── Step 1: Prerequisites ──
step "1/7" "Checking prerequisites..."

missing=""
command -v cargo >/dev/null 2>&1 || missing="$missing Rust"
command -v adb >/dev/null 2>&1   || missing="$missing ADB"

if [ -n "$missing" ]; then
    fail "Missing:$missing"
    exit 1
fi
ok "Rust toolchain found ($(cargo --version))"
ok "ADB found ($(adb --version | head -1))"

# ── Step 2: Build PC Daemon ──
step "2/7" "Building PC daemon (Rust)..."
cd "$PROJECT_ROOT/pc_daemon"
cargo build --release 2>&1 | sed 's/^/  /'
ok "Daemon built: target/release/phantom-core-daemon"
cd "$PROJECT_ROOT"

# ── Step 3: Build Proxy APK ──
step "3/7" "Building Proxy APK..."
if [ -f "$PROJECT_ROOT/android/proxy_app/gradlew" ]; then
    cd "$PROJECT_ROOT/android/proxy_app"
    chmod +x gradlew
    ./gradlew assembleDebug 2>&1 | tail -5 | sed 's/^/  /'
    ok "Proxy APK built"
    cd "$PROJECT_ROOT"
else
    warn "gradlew not found — run 'gradle wrapper' first"
fi

# ── Step 4: Build Demo APK ──
step "4/7" "Building Demo APK..."
if [ -f "$PROJECT_ROOT/android/demo_app/gradlew" ]; then
    cd "$PROJECT_ROOT/android/demo_app"
    chmod +x gradlew
    ./gradlew assembleDebug 2>&1 | tail -5 | sed 's/^/  /'
    ok "Demo APK built"
    cd "$PROJECT_ROOT"
else
    warn "gradlew not found — run 'gradle wrapper' first"
fi

# ── Step 5: Install APKs ──
step "5/7" "Installing APKs via ADB..."
PROXY_APK=$(find "$PROJECT_ROOT/android/proxy_app" -name "*.apk" -path "*/debug/*" 2>/dev/null | head -1)
DEMO_APK=$(find "$PROJECT_ROOT/android/demo_app" -name "*.apk" -path "*/debug/*" 2>/dev/null | head -1)

[ -n "$PROXY_APK" ] && { adb $ADB_ARGS install -r "$PROXY_APK" 2>&1; ok "Proxy installed"; } || warn "Proxy APK not found"
[ -n "$DEMO_APK" ]  && { adb $ADB_ARGS install -r "$DEMO_APK" 2>&1; ok "Demo installed"; }  || warn "Demo APK not found"

# ── Step 6: Push eBPF module ──
step "6/7" "Pushing eBPF module..."
adb $ADB_ARGS shell mkdir -p /data/local/tmp/phantom/ 2>/dev/null || true
adb $ADB_ARGS push "$PROJECT_ROOT/android/kernel_module/ebpf_program.c" /data/local/tmp/phantom/
adb $ADB_ARGS push "$PROJECT_ROOT/android/kernel_module/load.sh" /data/local/tmp/phantom/
ok "eBPF files pushed"
warn "Run 'adb shell su -c sh /data/local/tmp/phantom/load.sh' to load (root required)"

# ── Step 7: Start Daemon ──
step "7/7" "Starting PC daemon..."

# Detect local IP
LOCAL_IP=$(hostname -I 2>/dev/null | awk '{print $1}' || ifconfig | grep -Eo 'inet (addr:)?([0-9]*\.){3}[0-9]*' | grep -v 127.0.0.1 | head -1 | awk '{print $2}')

echo ""
echo -e "${C_GREEN}══════════════════════════════════════════${C_RESET}"
echo -e "${C_GREEN}  DEPLOYMENT COMPLETE${C_RESET}"
echo -e "${C_GREEN}══════════════════════════════════════════${C_RESET}"
echo ""
echo -e "  ${C_YELLOW}PC IP Address: $LOCAL_IP${C_RESET}"
echo -e "  ${C_YELLOW}Daemon Port:   $DAEMON_PORT${C_RESET}"
echo ""
echo "  Enter this IP in the PhantomCore app, then tap TETHER!"
echo ""
echo "  Starting daemon now..."

exec "$PROJECT_ROOT/pc_daemon/target/release/phantom-core-daemon"
