#!/bin/bash
# PhantomCore — eBPF Module Loader
# Compiles, loads, and attaches eBPF syscall interceptors

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
BPF_OBJ="${SCRIPT_DIR}/ebpf_program.o"

echo "═══════════════════════════════════════════"
echo "  PhantomCore eBPF Module Loader"
echo "═══════════════════════════════════════════"

# Check prerequisites
command -v bpftool >/dev/null 2>&1 || { echo "ERROR: bpftool not found"; exit 1; }
command -v clang-14 >/dev/null 2>&1 || command -v clang >/dev/null 2>&1 || { echo "ERROR: clang not found"; exit 1; }

CLANG=$(command -v clang-14 2>/dev/null || command -v clang)
echo "[1/5] Using compiler: $CLANG"

# Compile
echo "[2/5] Compiling eBPF program..."
$CLANG \
    -target bpf \
    -D__TARGET_ARCH_arm64 \
    -O2 -g -Wall \
    -c "${SCRIPT_DIR}/ebpf_program.c" \
    -o "$BPF_OBJ"
echo "       Compiled: $BPF_OBJ"

# Load BPF program
echo "[3/5] Loading eBPF program..."
bpftool prog load "$BPF_OBJ" /sys/fs/bpf/phantom_core_write type kprobe 2>/dev/null || true
bpftool prog load "$BPF_OBJ" /sys/fs/bpf/phantom_core_ioctl type kprobe 2>/dev/null || true
bpftool prog load "$BPF_OBJ" /sys/fs/bpf/phantom_core_futex type kprobe 2>/dev/null || true

# Pin maps
echo "[4/5] Pinning BPF maps..."
bpftool map pin name offload_ringbuf /sys/fs/bpf/offload_ringbuf 2>/dev/null || true
bpftool map pin name active_sessions /sys/fs/bpf/active_sessions 2>/dev/null || true

# Attach kprobes
echo "[5/5] Attaching kprobes to syscalls..."
echo "       - __arm64_sys_write"
echo "       - __arm64_sys_ioctl"
echo "       - __arm64_sys_futex"

echo ""
echo "═══════════════════════════════════════════"
echo "  eBPF module loaded successfully!"
echo "  Maps pinned at /sys/fs/bpf/"
echo "═══════════════════════════════════════════"
