// ============================================================================
// PhantomCore — Network Protocol Implementation
// Binary serialization matching the RFC spec in protocol/spec.md
// ============================================================================

use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use std::io::Cursor;
use thiserror::Error;

/// Protocol header size in bytes
pub const HEADER_SIZE: usize = 16;
/// Maximum payload size
#[allow(dead_code)]
pub const MAX_PAYLOAD: usize = 65000;
/// Flag: this packet is FEC parity
#[allow(dead_code)]
pub const FLAG_FEC_PARITY: u16 = 0x0001;
/// Flag: this is a speculative pre-execution hint
pub const FLAG_SPECULATIVE: u16 = 0x0002;
/// Flag: this is a NACK packet
pub const FLAG_NACK: u16 = 0x0004;
/// Flag: response contains delta-compressed data
pub const FLAG_DELTA: u16 = 0x0008;

#[derive(Error, Debug)]
#[allow(dead_code)]
pub enum ProtocolError {
    #[error("Packet too short: got {0} bytes, need at least {1}")]
    PacketTooShort(usize, usize),
    #[error("Payload length mismatch: header says {0}, got {1}")]
    PayloadMismatch(u32, usize),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

/// An offload request from the Android proxy
#[derive(Debug, Clone)]
pub struct OffloadRequest {
    pub session_id: u32,
    pub seq: u32,
    pub func_id: u32,
    pub flags: u16,
    pub payload: Vec<u8>,
    /// ARM64 general-purpose registers X0-X30
    pub registers: [u64; 31],
    /// Program counter at time of interception
    pub pc: u64,
}

/// Memory region that was modified during execution
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MemoryDelta {
    pub address: u64,
    pub data: Vec<u8>,
}

/// Result of executing a translated block on the PC
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ExecutionResult {
    pub session_id: u32,
    pub seq: u32,
    pub registers: [u64; 31],
    pub memory_deltas: Vec<MemoryDelta>,
    pub return_value: i64,
    /// Bitmask: bit N = register N was modified
    pub dirty_reg_bitmap: u32,
}

/// Parse a raw UDP packet into an OffloadRequest
pub fn parse_request(data: &[u8]) -> Result<OffloadRequest, ProtocolError> {
    if data.len() < HEADER_SIZE {
        return Err(ProtocolError::PacketTooShort(data.len(), HEADER_SIZE));
    }

    let mut cursor = Cursor::new(data);

    let session_id = cursor.read_u32::<LittleEndian>()?;
    let seq = cursor.read_u32::<LittleEndian>()?;
    let func_id = cursor.read_u32::<LittleEndian>()?;
    let flags = cursor.read_u16::<LittleEndian>()?;
    let payload_len = cursor.read_u16::<LittleEndian>()? as u32;

    // After the 16-byte header, we expect:
    //   31 * 8 bytes for registers = 248 bytes
    //   8 bytes for PC
    //   payload_len bytes for payload
    let registers_size = 31 * 8;
    let pc_size = 8;
    let expected_body = registers_size + pc_size + payload_len as usize;
    let remaining = data.len() - HEADER_SIZE;

    if remaining < expected_body {
        // If we don't have register data, create a minimal request
        // This handles lightweight control packets (NACKs, keepalives)
        let payload_start = HEADER_SIZE;
        let payload_end = data.len().min(payload_start + payload_len as usize);
        return Ok(OffloadRequest {
            session_id,
            seq,
            func_id,
            flags,
            payload: data[payload_start..payload_end].to_vec(),
            registers: [0u64; 31],
            pc: 0,
        });
    }

    // Read registers
    let mut registers = [0u64; 31];
    for reg in registers.iter_mut() {
        *reg = cursor.read_u64::<LittleEndian>()?;
    }

    let pc = cursor.read_u64::<LittleEndian>()?;

    // Read payload
    let pos = cursor.position() as usize;
    let payload = data[pos..pos + payload_len as usize].to_vec();

    Ok(OffloadRequest {
        session_id,
        seq,
        func_id,
        flags,
        payload,
        registers,
        pc,
    })
}

/// Serialize an ExecutionResult into a response packet
pub fn serialize_response(result: &ExecutionResult) -> Vec<u8> {
    let mut buf = Vec::with_capacity(512);

    // Header
    buf.write_u32::<LittleEndian>(result.session_id).unwrap();
    buf.write_u32::<LittleEndian>(result.seq).unwrap();
    buf.write_u32::<LittleEndian>(0).unwrap(); // func_id = 0 for responses
    buf.write_u16::<LittleEndian>(FLAG_DELTA).unwrap();

    // Placeholder for payload_len — we'll fill it at the end
    let payload_len_offset = buf.len();
    buf.write_u16::<LittleEndian>(0).unwrap();

    let payload_start = buf.len();

    // Return value
    buf.write_i64::<LittleEndian>(result.return_value).unwrap();

    // Dirty register bitmap
    buf.write_u32::<LittleEndian>(result.dirty_reg_bitmap).unwrap();

    // Only write registers that are dirty
    for i in 0..31 {
        if result.dirty_reg_bitmap & (1 << i) != 0 {
            buf.write_u64::<LittleEndian>(result.registers[i]).unwrap();
        }
    }

    // Memory deltas count
    buf.write_u32::<LittleEndian>(result.memory_deltas.len() as u32).unwrap();

    // Each delta: address (u64) + len (u32) + data
    for delta in &result.memory_deltas {
        buf.write_u64::<LittleEndian>(delta.address).unwrap();
        buf.write_u32::<LittleEndian>(delta.data.len() as u32).unwrap();
        buf.extend_from_slice(&delta.data);
    }

    // Fill in payload_len
    let payload_len = (buf.len() - payload_start) as u16;
    buf[payload_len_offset] = (payload_len & 0xFF) as u8;
    buf[payload_len_offset + 1] = (payload_len >> 8) as u8;

    buf
}

/// Delta-compress a result against the previous session state
pub fn compute_delta(
    result: &ExecutionResult,
    prev_registers: &[u64; 31],
) -> ExecutionResult {
    let mut dirty_bitmap: u32 = 0;
    let mut delta_regs = [0u64; 31];

    for i in 0..31 {
        if result.registers[i] != prev_registers[i] {
            dirty_bitmap |= 1 << i;
            delta_regs[i] = result.registers[i];
        }
    }

    ExecutionResult {
        session_id: result.session_id,
        seq: result.seq,
        registers: delta_regs,
        memory_deltas: result.memory_deltas.clone(),
        return_value: result.return_value,
        dirty_reg_bitmap: dirty_bitmap,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_roundtrip_response() {
        let result = ExecutionResult {
            session_id: 42,
            seq: 7,
            registers: {
                let mut r = [0u64; 31];
                r[0] = 0xDEADBEEF;
                r[1] = 0xCAFEBABE;
                r
            },
            memory_deltas: vec![MemoryDelta {
                address: 0x7000_0000,
                data: vec![1, 2, 3, 4],
            }],
            return_value: 0,
            dirty_reg_bitmap: 0b11, // X0 and X1
        };

        let serialized = serialize_response(&result);
        assert!(serialized.len() > HEADER_SIZE);

        // Verify header
        let mut c = Cursor::new(&serialized);
        assert_eq!(c.read_u32::<LittleEndian>().unwrap(), 42);
        assert_eq!(c.read_u32::<LittleEndian>().unwrap(), 7);
    }

    #[test]
    fn test_delta_compression() {
        let prev = [100u64; 31];
        let mut current_regs = [100u64; 31];
        current_regs[0] = 999;
        current_regs[5] = 777;

        let result = ExecutionResult {
            session_id: 1,
            seq: 1,
            registers: current_regs,
            memory_deltas: vec![],
            return_value: 0,
            dirty_reg_bitmap: 0xFFFFFFFF,
        };

        let delta = compute_delta(&result, &prev);
        // Only X0 and X5 should be dirty
        assert_eq!(delta.dirty_reg_bitmap, (1 << 0) | (1 << 5));
        assert_eq!(delta.registers[0], 999);
        assert_eq!(delta.registers[5], 777);
        assert_eq!(delta.registers[1], 0); // Not dirty, zeroed
    }
}
