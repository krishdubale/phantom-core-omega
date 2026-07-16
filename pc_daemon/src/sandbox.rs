// ============================================================================
// PhantomCore — Sandboxed Execution Environment
// Memory shadow, register shadow, JIT cache, speculative execution buffer
// ============================================================================

use crate::protocol::{ExecutionResult, MemoryDelta};
use std::collections::HashMap;

/// Page size for memory shadow tracking (4KB)
pub const PAGE_SIZE: usize = 4096;
/// Cache line size for delta compression (64 bytes)
#[allow(dead_code)]
pub const CACHE_LINE_SIZE: usize = 64;
/// Maximum speculative cache entries before eviction
const MAX_SPECULATIVE_ENTRIES: usize = 256;

/// Complete execution state for one offloaded session
#[derive(Debug)]
pub struct SessionState {
    /// Shadow copy of ARM64 registers X0-X30
    pub register_shadow: [u64; 31],
    /// Shadow copy of ARM64 program counter
    #[allow(dead_code)]
    pub pc_shadow: u64,
    /// Virtual memory shadow — maps page-aligned addresses to page data
    pub memory_shadow: HashMap<u64, Vec<u8>>,
    /// JIT translation cache — maps ARM64 PC to compiled x86_64 machine code
    pub jit_cache: HashMap<u64, JitBlock>,
    /// Speculative execution results — maps predicted seq numbers to results
    speculative_cache: HashMap<u32, ExecutionResult>,
    /// Tracks the order of speculative entries for LRU eviction
    speculative_order: Vec<u32>,
    /// Total instructions translated in this session
    pub instructions_translated: u64,
    /// Total instructions executed in this session
    pub instructions_executed: u64,
}

/// A compiled JIT block — translated ARM64 basic block in x86_64 machine code
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct JitBlock {
    /// The compiled x86_64 machine code bytes
    pub code: Vec<u8>,
    /// Number of ARM64 instructions in this block
    pub arm64_insn_count: usize,
    /// The ARM64 PC where this block starts
    pub start_pc: u64,
    /// The ARM64 PC where this block ends (exclusive)
    pub end_pc: u64,
    /// How many times this block has been executed (for hot-path optimization)
    pub execution_count: u64,
}

impl SessionState {
    /// Create a new blank session state
    pub fn new() -> Self {
        SessionState {
            register_shadow: [0u64; 31],
            pc_shadow: 0,
            memory_shadow: HashMap::with_capacity(1024),
            jit_cache: HashMap::with_capacity(512),
            speculative_cache: HashMap::with_capacity(MAX_SPECULATIVE_ENTRIES),
            speculative_order: Vec::with_capacity(MAX_SPECULATIVE_ENTRIES),
            instructions_translated: 0,
            instructions_executed: 0,
        }
    }

    /// Check if we have a pre-computed speculative result for this sequence number
    pub fn check_speculative_cache(&mut self, seq: u32) -> Option<ExecutionResult> {
        if let Some(result) = self.speculative_cache.remove(&seq) {
            self.speculative_order.retain(|&s| s != seq);
            log::info!("Speculative cache HIT for seq {}", seq);
            Some(result)
        } else {
            log::trace!("Speculative cache MISS for seq {}", seq);
            None
        }
    }

    /// Store a speculative execution result, evicting oldest if cache is full
    pub fn store_speculative(&mut self, seq: u32, result: ExecutionResult) {
        // LRU eviction if at capacity
        if self.speculative_cache.len() >= MAX_SPECULATIVE_ENTRIES {
            if let Some(oldest_seq) = self.speculative_order.first().copied() {
                self.speculative_cache.remove(&oldest_seq);
                self.speculative_order.remove(0);
                log::trace!("Evicted speculative entry seq {}", oldest_seq);
            }
        }
        self.speculative_cache.insert(seq, result);
        self.speculative_order.push(seq);
    }

    /// Apply an execution result to update the session shadows
    pub fn apply_result(&mut self, result: &ExecutionResult) {
        // Update register shadow — only dirty registers
        for i in 0..31 {
            if result.dirty_reg_bitmap & (1 << i) != 0 {
                self.register_shadow[i] = result.registers[i];
            }
        }

        // Apply memory deltas to the shadow
        for delta in &result.memory_deltas {
            self.write_memory(delta.address, &delta.data);
        }

        self.instructions_executed += 1;
    }

    /// Read from the memory shadow. Returns zeros for uninitialized pages.
    #[allow(dead_code)]
    pub fn read_memory(&self, address: u64, size: usize) -> Vec<u8> {
        let mut result = vec![0u8; size];
        let mut offset = 0usize;
        let mut addr = address;

        while offset < size {
            let page_base = addr & !(PAGE_SIZE as u64 - 1);
            let page_offset = (addr - page_base) as usize;
            let bytes_in_page = (PAGE_SIZE - page_offset).min(size - offset);

            if let Some(page_data) = self.memory_shadow.get(&page_base) {
                let src_end = (page_offset + bytes_in_page).min(page_data.len());
                let copy_len = src_end.saturating_sub(page_offset);
                if copy_len > 0 {
                    result[offset..offset + copy_len]
                        .copy_from_slice(&page_data[page_offset..page_offset + copy_len]);
                }
            }
            // If no page exists, the result stays zeroed (demand-zero paging)

            offset += bytes_in_page;
            addr += bytes_in_page as u64;
        }

        result
    }

    /// Write to the memory shadow, creating pages on demand
    pub fn write_memory(&mut self, address: u64, data: &[u8]) {
        let mut offset = 0usize;
        let mut addr = address;

        while offset < data.len() {
            let page_base = addr & !(PAGE_SIZE as u64 - 1);
            let page_offset = (addr - page_base) as usize;
            let bytes_in_page = (PAGE_SIZE - page_offset).min(data.len() - offset);

            let page = self
                .memory_shadow
                .entry(page_base)
                .or_insert_with(|| vec![0u8; PAGE_SIZE]);

            page[page_offset..page_offset + bytes_in_page]
                .copy_from_slice(&data[offset..offset + bytes_in_page]);

            offset += bytes_in_page;
            addr += bytes_in_page as u64;
        }
    }

    /// Get a JIT block from cache, or None if not yet translated
    pub fn get_jit_block(&self, pc: u64) -> Option<&JitBlock> {
        self.jit_cache.get(&pc)
    }

    /// Store a translated JIT block in the cache
    pub fn store_jit_block(&mut self, pc: u64, block: JitBlock) {
        self.instructions_translated += block.arm64_insn_count as u64;
        self.jit_cache.insert(pc, block);
    }

    /// Get memory deltas between current state and a snapshot of addresses
    #[allow(dead_code)]
    pub fn compute_memory_deltas(&self, watched_addresses: &[(u64, usize)]) -> Vec<MemoryDelta> {
        let mut deltas = Vec::new();
        for &(addr, size) in watched_addresses {
            let data = self.read_memory(addr, size);
            if data.iter().any(|&b| b != 0) {
                deltas.push(MemoryDelta {
                    address: addr,
                    data,
                });
            }
        }
        deltas
    }

    /// Returns session statistics as a JSON-friendly string
    pub fn stats_json(&self) -> String {
        serde_json::json!({
            "instructions_translated": self.instructions_translated,
            "instructions_executed": self.instructions_executed,
            "jit_cache_entries": self.jit_cache.len(),
            "memory_pages": self.memory_shadow.len(),
            "speculative_cache_size": self.speculative_cache.len(),
        })
        .to_string()
    }
}

impl Default for SessionState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_memory_read_write() {
        let mut state = SessionState::new();
        let data = vec![0xDE, 0xAD, 0xBE, 0xEF];
        state.write_memory(0x1000, &data);
        let read_back = state.read_memory(0x1000, 4);
        assert_eq!(read_back, data);
    }

    #[test]
    fn test_memory_cross_page() {
        let mut state = SessionState::new();
        let addr = PAGE_SIZE as u64 - 2; // 2 bytes before page boundary
        let data = vec![1, 2, 3, 4]; // crosses into next page
        state.write_memory(addr, &data);
        let read_back = state.read_memory(addr, 4);
        assert_eq!(read_back, data);
    }

    #[test]
    fn test_speculative_cache_lru() {
        let mut state = SessionState::new();
        // Fill cache
        for i in 0..MAX_SPECULATIVE_ENTRIES as u32 {
            state.store_speculative(
                i,
                ExecutionResult {
                    session_id: 1,
                    seq: i,
                    registers: [0; 31],
                    memory_deltas: vec![],
                    return_value: i as i64,
                    dirty_reg_bitmap: 0,
                },
            );
        }
        // Adding one more should evict seq 0
        state.store_speculative(
            999,
            ExecutionResult {
                session_id: 1,
                seq: 999,
                registers: [0; 31],
                memory_deltas: vec![],
                return_value: 999,
                dirty_reg_bitmap: 0,
            },
        );
        assert!(state.check_speculative_cache(0).is_none());
        assert!(state.check_speculative_cache(999).is_some());
    }
}
