# PhantomCore Protocol Specification v1.0

## 1. Overview

The PhantomCore network protocol enables transparent offloading of ARM64 syscalls from an Android device to a PC over UDP. The protocol is designed for sub-10ms latency with built-in redundancy via Forward Error Correction (FEC) and NACK-based retransmission.

**Design Goals:**
- Ultra-low latency (< 10ms average round-trip)
- Resilience to packet loss (up to 12.5% loss tolerable with FEC)
- Minimal bandwidth overhead via delta compression
- Speculative execution support to hide network latency

## 2. Packet Format

All integers are little-endian. Maximum UDP payload: 65000 bytes.

### 2.1 Request Packet (Phone → PC)

```
 0                   1                   2                   3
 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|                         session_id                            |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|                           seq_num                             |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|                           func_id                             |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|            flags              |         payload_len           |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|                     registers X0-X30                          |
|                       (31 x 8 bytes = 248 bytes)              |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|                     program_counter (8 bytes)                 |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|                     payload (variable)                        |
|                     (ARM64 basic block bytes)                 |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
```

### 2.2 Response Packet (PC → Phone)

```
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|                         session_id                            |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|                           seq_num                             |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|                           func_id (0)                         |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|     flags (FLAG_DELTA)        |         payload_len           |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|                      return_value (i64)                       |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|                  dirty_reg_bitmap (u32)                        |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|              dirty registers (8 bytes each)                   |
|              (only registers where bit N is set)              |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|                    delta_count (u32)                           |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|     For each delta:                                           |
|       address (u64) | length (u32) | data (variable)          |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
```

### 2.3 Flags Field

| Bit | Name | Description |
|-----|------|-------------|
| 0 | FLAG_FEC_PARITY | This packet is an FEC parity packet |
| 1 | FLAG_SPECULATIVE | This is a speculative pre-execution hint |
| 2 | FLAG_NACK | This is a NACK retransmission request |
| 3 | FLAG_DELTA | Response contains delta-compressed data |

## 3. Session Management

### 3.1 Handshake
No explicit handshake. The first packet from a new `session_id` implicitly creates a session on the daemon. Sessions are keyed by `(source_ip, source_port, session_id)`.

### 3.2 Keepalive
If no packets are received for 30 seconds, the session is garbage collected. The proxy should send a keepalive (empty payload, seq=0) every 10 seconds.

## 4. Forward Error Correction (FEC)

### 4.1 Algorithm
For every 8 consecutive data packets, 2 parity packets are generated:

```
Parity1 = Packet[0] XOR Packet[1] XOR Packet[2] XOR Packet[3]
Parity2 = Packet[4] XOR Packet[5] XOR Packet[6] XOR Packet[7]
```

### 4.2 Recovery
Each parity packet can recover exactly 1 lost packet in its group of 4. If packet[i] is lost:

```
Packet[i] = Parity XOR Packet[j] XOR Packet[k] XOR Packet[l]
    where j,k,l are the other 3 packets in the group
```

### 4.3 Padding
All packets in an FEC block are XOR'd byte-by-byte. Shorter packets are zero-padded to match the longest packet in the block.

## 5. NACK-based Retransmission

### 5.1 Gap Detection
The receiver tracks sequence numbers. When a gap is detected (e.g., received seq 5 then seq 8, missing 6 and 7), the missing numbers are queued for NACK.

### 5.2 NACK Packet Format
```
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|       missing_count (u32)     |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|       seq_num_1 (u32)         |
|       seq_num_2 (u32)         |
|       ...                     |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
```

### 5.3 Timing
NACKs are sent every 5ms if there are outstanding gaps. The sender retransmits the requested packets immediately upon receiving a NACK.

## 6. Delta Compression

### 6.1 Register Delta
The response includes a `dirty_reg_bitmap` (32-bit). Only registers with their corresponding bit set are included in the payload, saving up to 87% bandwidth for typical syscalls that modify 1-4 registers.

### 6.2 Memory Delta
Memory changes are sent as a list of `(address, length, data)` tuples. Only pages that were actually written during execution are included.

## 7. Speculative Execution Hints

### 7.1 LSTM Predictions
The phone-side LSTM predictor observes the stream of `(func_id, PC)` pairs and predicts the next 3 likely calls. These are sent as packets with `FLAG_SPECULATIVE` set.

### 7.2 Pre-execution
The PC daemon pre-translates and pre-executes speculative requests, storing results in a speculative cache. When the actual request arrives matching a prediction, the response is sent instantly (0ms additional latency).

### 7.3 Cache Eviction
The speculative cache uses LRU eviction with a maximum of 256 entries.

## 8. Security Considerations

- All execution occurs in a sandboxed memory model (no real memory access on the PC)
- The daemon binds to `0.0.0.0:42069` — in production, restrict to the local subnet
- No authentication in v1.0 — intended for trusted LAN use only
- Session isolation: each session has independent register/memory shadows

## 9. Reference Implementation Notes

- **Android (C++)**: See `android/proxy_app/app/src/main/cpp/proxy_native.cpp`
- **PC (Rust)**: See `pc_daemon/src/protocol.rs`, `fec.rs`, `nack.rs`
- Both implementations are fully compatible and tested against each other
