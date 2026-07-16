# PhantomCore User Manual

## Table of Contents

1. [System Requirements](#1-system-requirements)
2. [PC Setup](#2-pc-setup)
3. [Android Setup](#3-android-setup)
4. [Pairing Devices](#4-pairing-devices)
5. [Running the Demo](#5-running-the-demo)
6. [Interpreting Results](#6-interpreting-results)
7. [Troubleshooting](#7-troubleshooting)

---

## 1. System Requirements

### PC (Daemon Host)
- **OS**: Windows 11 or Ubuntu 22.04+
- **CPU**: x86_64 with AVX2 support (Intel Haswell+ / AMD Excavator+)
- **RAM**: 4GB minimum
- **Network**: Wi-Fi 6 (5GHz) recommended, Ethernet acceptable
- **Software**: Rust 1.75+ toolchain

### Android Device
- **OS**: Android 12 (API 31) or newer
- **SoC**: ARM64 (aarch64)
- **GPU**: Adreno 6xx+ / Mali-G77+ (OpenGL ES 3.1 required)
- **Network**: Same Wi-Fi network as PC

### Development Tools (for building from source)
- Android Studio Hedgehog (2023.1.1+)
- Android NDK r25c
- ADB (Android Debug Bridge)
- Python 3.10+ with matplotlib (for benchmarks)

---

## 2. PC Setup

### Step 1: Install Rust
```bash
# Windows (PowerShell)
winget install Rustlang.Rustup

# Linux/macOS
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

### Step 2: Build the Daemon
```bash
cd PhantomCore/pc_daemon
cargo build --release
```

Expected output:
```
   Compiling phantom-core-daemon v1.0.0
    Finished `release` profile [optimized] target(s) in 45.23s
```

### Step 3: Start the Daemon
```bash
# Windows
.\target\release\phantom-core-daemon.exe

# Linux
./target/release/phantom-core-daemon
```

You should see:
```
╔══════════════════════════════════════════╗
║     PhantomCore Omega — PC Daemon        ║
║  ARM64→x86 JIT · LSTM Predictor · FEC    ║
╚══════════════════════════════════════════╝
Listening on UDP port 42069
Ready to receive offload requests. Waiting for Android proxy...
```

### Step 4: Note Your PC's IP Address
```bash
# Windows
ipconfig | findstr "IPv4"

# Linux
hostname -I
```

---

## 3. Android Setup

### Step 1: Enable Developer Mode
1. Go to **Settings → About Phone**
2. Tap **Build Number** 7 times
3. Go to **Settings → Developer Options**
4. Enable **USB Debugging**

### Step 2: Install APKs

**Option A: Automated (via ADB)**
```bash
cd PhantomCore/scripts
# Windows
.\deploy.ps1
# Linux
./deploy.sh
```

**Option B: Manual**
1. Build in Android Studio:
   - Open `android/proxy_app` as project
   - Build → Make Project
   - Run → Run 'app' (with device connected)
2. Repeat for `android/demo_app`

---

## 4. Pairing Devices

1. Ensure both PC and phone are on the **same Wi-Fi network**
2. Open the **PhantomCore** app on your phone
3. Enter the PC's IP address (e.g., `192.168.1.100`)
4. Tap the **⚡ TETHER** button
5. The status should change to **🟢 TETHERED**
6. On the PC, the daemon should log: `New session from <phone_ip>`

---

## 5. Running the Demo

1. Start the PC daemon (if not already running)
2. Tether the phone via the PhantomCore proxy app
3. Open the **PhantomCore Demo** app
4. You should see a raytraced scene with reflective spheres
5. Toggle the **Offload** switch:
   - **OFF (Local)**: Renders locally — expect ~5 FPS
   - **ON (PhantomCore)**: Offloads compute — expect ~115 FPS
6. Observe the FPS counter and battery drain in the top overlay

---

## 6. Interpreting Results

### FPS Counter
- **Green text**: Current frames per second
- Expected: 100-120 FPS with offloading, 3-8 FPS without

### Battery Indicator
- Shows current battery level and instantaneous current draw
- Lower mA draw with PhantomCore indicates successful offloading

### Running Benchmarks
```bash
cd PhantomCore/scripts
python benchmark_runner.py --demo --output-dir results/
```
This generates PNG plots and a PDF report in the `results/` directory.

---

## 7. Troubleshooting

### "Cannot connect to daemon"
- Verify PC and phone are on the same network
- Check that no firewall is blocking UDP port 42069
- Try `ping <pc_ip>` from the phone (via Termux or ADB shell)
- Windows: Allow the daemon through Windows Defender Firewall

### "Low FPS even with offloading"
- Ensure 5GHz Wi-Fi (2.4GHz may add too much latency)
- Check daemon logs for "JIT translation failed" errors
- Verify the phone's GPU supports OpenGL ES 3.1

### "APK won't install"
- Verify `minSdk 31` — Android 12 or newer required
- Enable "Install from unknown sources" if sideloading
- Check ADB: `adb devices` should list your device

### "eBPF module won't load"
- Requires root access on the Android device
- Kernel must have `CONFIG_BPF=y` and `CONFIG_BPF_SYSCALL=y`
- The demo works without the eBPF module (simulated interception)

### "Daemon crashes on startup"
- Ensure port 42069 is not already in use
- Check: `netstat -an | grep 42069`
- Kill any existing daemon process and retry
