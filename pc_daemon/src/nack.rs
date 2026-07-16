// ============================================================================
// PhantomCore — NACK-based Retransmission Tracker
// Timer-driven, sends NACKs every 5ms for missing sequence numbers
// ============================================================================

use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use std::collections::HashSet;
use std::io::Cursor;
use std::time::{Duration, Instant};

/// NACK interval — send NACKs for missing packets every 5ms
pub const NACK_INTERVAL: Duration = Duration::from_millis(5);
/// Maximum gap we'll track before assuming we're out of sync
const MAX_GAP: u32 = 1024;

/// Tracks received sequence numbers and detects gaps for NACK generation
#[derive(Debug)]
pub struct NackTracker {
    /// Set of sequence numbers we've received
    received: HashSet<u32>,
    /// The highest sequence number we've seen
    highest_seq: u32,
    /// The lowest sequence number we're still waiting for
    lowest_pending: u32,
    /// When we last sent a NACK
    last_nack_time: Instant,
    /// How often to send NACKs
    nack_interval: Duration,
    /// Sequence numbers we've already NACKed (to avoid duplicate retransmissions)
    nacked: HashSet<u32>,
}

impl NackTracker {
    pub fn new() -> Self {
        NackTracker {
            received: HashSet::with_capacity(2048),
            highest_seq: 0,
            lowest_pending: 0,
            last_nack_time: Instant::now(),
            nack_interval: NACK_INTERVAL,
            nacked: HashSet::with_capacity(256),
        }
    }

    /// Record that we received a packet with this sequence number
    pub fn record_received(&mut self, seq: u32) {
        self.received.insert(seq);

        if seq > self.highest_seq {
            self.highest_seq = seq;
        }

        // Advance the lowest_pending watermark
        while self.received.contains(&self.lowest_pending) {
            self.received.remove(&self.lowest_pending);
            self.nacked.remove(&self.lowest_pending);
            self.lowest_pending += 1;
        }
    }

    /// Get the list of sequence numbers that are missing (gaps in the received set)
    pub fn get_missing(&self) -> Vec<u32> {
        let mut missing = Vec::new();
        let upper_bound = self.highest_seq.min(self.lowest_pending + MAX_GAP);

        for seq in self.lowest_pending..=upper_bound {
            if !self.received.contains(&seq) {
                missing.push(seq);
            }
        }

        missing
    }

    /// Check if enough time has elapsed since the last NACK
    pub fn should_send_nack(&self) -> bool {
        self.last_nack_time.elapsed() >= self.nack_interval && !self.get_missing().is_empty()
    }

    /// Build a NACK packet containing the list of missing sequence numbers.
    /// Also updates the last_nack_time and tracks which seqs were NACKed.
    ///
    /// Format: [count: u32] [seq1: u32] [seq2: u32] ...
    pub fn build_nack_packet(&mut self) -> Vec<u8> {
        let missing = self.get_missing();
        let mut buf = Vec::with_capacity(4 + missing.len() * 4);

        buf.write_u32::<LittleEndian>(missing.len() as u32).unwrap();
        for &seq in &missing {
            buf.write_u32::<LittleEndian>(seq).unwrap();
            self.nacked.insert(seq);
        }

        self.last_nack_time = Instant::now();
        buf
    }

    /// Parse a received NACK packet into a list of missing sequence numbers
    #[allow(dead_code)]
    pub fn parse_nack_packet(data: &[u8]) -> Result<Vec<u32>, std::io::Error> {
        let mut cursor = Cursor::new(data);
        let count = cursor.read_u32::<LittleEndian>()?;
        let mut seqs = Vec::with_capacity(count as usize);

        for _ in 0..count {
            seqs.push(cursor.read_u32::<LittleEndian>()?);
        }

        Ok(seqs)
    }

    /// Number of packets currently considered missing
    #[allow(dead_code)]
    pub fn missing_count(&self) -> usize {
        self.get_missing().len()
    }

    /// Returns (lowest_pending, highest_seen) for diagnostics
    #[allow(dead_code)]
    pub fn window(&self) -> (u32, u32) {
        (self.lowest_pending, self.highest_seq)
    }
}

impl Default for NackTracker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_no_gaps() {
        let mut tracker = NackTracker::new();
        for i in 0..10 {
            tracker.record_received(i);
        }
        assert!(tracker.get_missing().is_empty());
    }

    #[test]
    fn test_single_gap() {
        let mut tracker = NackTracker::new();
        tracker.record_received(0);
        tracker.record_received(1);
        // Skip 2
        tracker.record_received(3);
        tracker.record_received(4);

        let missing = tracker.get_missing();
        assert_eq!(missing, vec![2]);
    }

    #[test]
    fn test_multiple_gaps() {
        let mut tracker = NackTracker::new();
        tracker.record_received(0);
        // Skip 1, 2
        tracker.record_received(3);
        // Skip 4
        tracker.record_received(5);

        let missing = tracker.get_missing();
        assert_eq!(missing, vec![1, 2, 4]);
    }

    #[test]
    fn test_nack_packet_roundtrip() {
        let mut tracker = NackTracker::new();
        tracker.record_received(0);
        tracker.record_received(3);
        tracker.record_received(5);

        let nack_data = tracker.build_nack_packet();
        let parsed = NackTracker::parse_nack_packet(&nack_data).unwrap();
        assert_eq!(parsed, vec![1, 2, 4]);
    }

    #[test]
    fn test_watermark_advances() {
        let mut tracker = NackTracker::new();
        tracker.record_received(0);
        tracker.record_received(2);
        tracker.record_received(3);
        // Gap at 1
        assert_eq!(tracker.window().0, 1);
        // Fill the gap
        tracker.record_received(1);
        // Watermark should advance past 0,1,2,3
        assert_eq!(tracker.window().0, 4);
    }
}
