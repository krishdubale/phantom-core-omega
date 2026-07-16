# PhantomCore PC Daemon

The PC-side daemon that receives offloaded ARM64 syscalls from Android devices, JIT-translates them to x86_64, and executes them in a sandboxed environment.

## Building

```bash
cargo build --release
```

Requires Rust 1.75+ with the 2021 edition.

## Usage

```bash
# Start with default settings (UDP :42069)
./target/release/phantom-core-daemon

# Environment variables:
#   RUST_LOG=debug    — verbose logging
#   RUST_LOG=trace    — maximum detail (per-packet)
```

## Architecture

```
UDP Packet In ──► Protocol Parser ──► Speculative Cache Check
                                              │
                                    ┌─────────┴─────────┐
                                    │ HIT               │ MISS
                                    ▼                   ▼
                              Send Cached         JIT Translate
                              Response            (ARM64 → x86)
                                                       │
                                                       ▼
                                                 Sandbox Execute
                                                       │
                                                       ▼
                                                 Delta Compress
                                                       │
                                                       ▼
                                              LSTM Predict Next 3
                                                       │
                                                       ▼
                                              Pre-translate & Cache
                                                       │
                                                       ▼
                                              Send Response (+ FEC)
```

## Modules

- **jit.rs** — ARM64 instruction decoder + x86_64 emitter
- **protocol.rs** — Binary packet serialization/deserialization
- **sandbox.rs** — Memory shadow, register file, JIT cache
- **predictor.rs** — Pure-Rust LSTM cell for speculative execution
- **fec.rs** — XOR-based forward error correction (2 parity / 8 data)
- **nack.rs** — Timer-driven NACK retransmission tracker
- **benchmark.rs** — Latency/throughput/percentile metrics collector

## Configuration

The daemon listens on `0.0.0.0:42069` by default. Statistics are logged every 10 seconds and saved to `phantom_benchmark.json` on shutdown (Ctrl+C).

## Testing

```bash
cargo test
```
