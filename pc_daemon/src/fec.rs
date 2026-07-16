// ============================================================================
// PhantomCore — Forward Error Correction (FEC)
// XOR-based parity: 2 parity packets per 8 data packets
// ============================================================================

/// Number of data packets per FEC block
pub const FEC_BLOCK_SIZE: usize = 8;
/// Number of parity packets per FEC block
#[allow(dead_code)]
pub const FEC_PARITY_COUNT: usize = 2;

/// XOR-based FEC encoder
/// Accumulates data packets and produces parity when a block is complete
#[derive(Debug)]
pub struct FecEncoder {
    /// Buffer of data packets in the current block
    block_buffer: Vec<Vec<u8>>,
    /// Maximum packet size seen in this block (for padding)
    max_packet_size: usize,
}

impl FecEncoder {
    pub fn new() -> Self {
        FecEncoder {
            block_buffer: Vec::with_capacity(FEC_BLOCK_SIZE),
            max_packet_size: 0,
        }
    }

    /// Add a data packet to the current FEC block.
    /// Returns Some(parity_packets) when the block is complete (8 packets accumulated).
    /// The returned Vec contains exactly 2 parity packets.
    pub fn add_packet(&mut self, data: &[u8]) -> Option<Vec<Vec<u8>>> {
        if data.len() > self.max_packet_size {
            self.max_packet_size = data.len();
        }
        self.block_buffer.push(data.to_vec());

        if self.block_buffer.len() == FEC_BLOCK_SIZE {
            let parities = self.generate_parity();
            self.block_buffer.clear();
            self.max_packet_size = 0;
            Some(parities)
        } else {
            None
        }
    }

    /// Generate 2 parity packets from the current block of 8 data packets.
    /// Parity1 = XOR(packets[0..4])
    /// Parity2 = XOR(packets[4..8])
    fn generate_parity(&self) -> Vec<Vec<u8>> {
        let size = self.max_packet_size;
        let mut parity1 = vec![0u8; size];
        let mut parity2 = vec![0u8; size];

        // Parity 1 covers first half of the block (packets 0-3)
        for i in 0..4 {
            xor_into(&mut parity1, &self.block_buffer[i]);
        }

        // Parity 2 covers second half of the block (packets 4-7)
        for i in 4..8 {
            xor_into(&mut parity2, &self.block_buffer[i]);
        }

        vec![parity1, parity2]
    }

    /// Flush any remaining packets (less than 8) with parity.
    /// Useful at end of stream.
    #[allow(dead_code)]
    pub fn flush(&mut self) -> Option<Vec<Vec<u8>>> {
        if self.block_buffer.is_empty() {
            return None;
        }
        // Pad with empty packets to fill the block
        while self.block_buffer.len() < FEC_BLOCK_SIZE {
            self.block_buffer.push(vec![0u8; self.max_packet_size.max(1)]);
        }
        let parities = self.generate_parity();
        self.block_buffer.clear();
        self.max_packet_size = 0;
        Some(parities)
    }
}

/// XOR-based FEC decoder
/// Recovers one missing packet per parity group (4 data + 1 parity)
#[derive(Debug)]
pub struct FecDecoder;

impl FecDecoder {
    pub fn new() -> Self {
        FecDecoder
    }

    /// Attempt to recover missing packets from a received block.
    ///
    /// `packets` — 8 slots, each either Some(data) or None (lost).
    /// `parities` — 2 parity packets (one for each half-block).
    ///
    /// Returns the full 8-packet block with recovered data where possible.
    /// If more than 1 packet is lost in a half-block, recovery is impossible
    /// for that half, and None entries remain.
    #[allow(dead_code)]
    pub fn decode(
        &self,
        packets: &[Option<Vec<u8>>; 8],
        parities: &[Vec<u8>; 2],
    ) -> [Option<Vec<u8>>; 8] {
        let mut result: [Option<Vec<u8>>; 8] = [
            packets[0].clone(),
            packets[1].clone(),
            packets[2].clone(),
            packets[3].clone(),
            packets[4].clone(),
            packets[5].clone(),
            packets[6].clone(),
            packets[7].clone(),
        ];

        // Try to recover first half (indices 0-3) using parity1
        Self::recover_half(&mut result, 0..4, &parities[0]);

        // Try to recover second half (indices 4-7) using parity2
        Self::recover_half(&mut result, 4..8, &parities[1]);

        result
    }

    /// Attempt recovery of one missing packet in a half-block
    #[allow(dead_code)]
    fn recover_half(
        result: &mut [Option<Vec<u8>>; 8],
        range: std::ops::Range<usize>,
        parity: &[u8],
    ) {
        let mut missing_idx: Option<usize> = None;
        let mut missing_count = 0;

        for i in range.clone() {
            if result[i].is_none() {
                missing_idx = Some(i);
                missing_count += 1;
            }
        }

        // Can only recover if exactly 1 packet is missing
        if missing_count != 1 {
            return;
        }

        let missing = missing_idx.unwrap();
        let mut recovered = parity.to_vec();

        // XOR all present packets against parity to recover the missing one
        for i in range {
            if i != missing {
                if let Some(ref data) = result[i] {
                    xor_into(&mut recovered, data);
                }
            }
        }

        result[missing] = Some(recovered);
    }
}

/// XOR `src` into `dst` byte-by-byte. `dst` is padded with zeros if shorter.
fn xor_into(dst: &mut Vec<u8>, src: &[u8]) {
    // Extend dst if src is longer
    if src.len() > dst.len() {
        dst.resize(src.len(), 0);
    }
    for (d, s) in dst.iter_mut().zip(src.iter()) {
        *d ^= *s;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fec_no_loss() {
        let mut encoder = FecEncoder::new();
        let packets: Vec<Vec<u8>> = (0..8).map(|i| vec![i as u8; 64]).collect();
        let mut parities = None;

        for pkt in &packets {
            parities = encoder.add_packet(pkt);
        }

        let parities = parities.expect("Should have parity after 8 packets");
        assert_eq!(parities.len(), 2);

        // No loss — all packets present
        let received: [Option<Vec<u8>>; 8] = [
            Some(packets[0].clone()),
            Some(packets[1].clone()),
            Some(packets[2].clone()),
            Some(packets[3].clone()),
            Some(packets[4].clone()),
            Some(packets[5].clone()),
            Some(packets[6].clone()),
            Some(packets[7].clone()),
        ];

        let decoder = FecDecoder::new();
        let result = decoder.decode(&received, &[parities[0].clone(), parities[1].clone()]);
        for i in 0..8 {
            assert_eq!(result[i].as_ref().unwrap(), &packets[i]);
        }
    }

    #[test]
    fn test_fec_single_loss_recovery() {
        let mut encoder = FecEncoder::new();
        let packets: Vec<Vec<u8>> = (0..8).map(|i| vec![(i * 17) as u8; 32]).collect();
        let mut parities = None;

        for pkt in &packets {
            parities = encoder.add_packet(pkt);
        }
        let parities = parities.unwrap();

        // Lose packet 2 (first half) and packet 6 (second half)
        let received: [Option<Vec<u8>>; 8] = [
            Some(packets[0].clone()),
            Some(packets[1].clone()),
            None, // packet 2 lost
            Some(packets[3].clone()),
            Some(packets[4].clone()),
            Some(packets[5].clone()),
            None, // packet 6 lost
            Some(packets[7].clone()),
        ];

        let decoder = FecDecoder::new();
        let result = decoder.decode(&received, &[parities[0].clone(), parities[1].clone()]);

        // Both should be recovered
        assert_eq!(result[2].as_ref().unwrap(), &packets[2]);
        assert_eq!(result[6].as_ref().unwrap(), &packets[6]);
    }

    #[test]
    fn test_fec_double_loss_same_half() {
        let mut encoder = FecEncoder::new();
        let packets: Vec<Vec<u8>> = (0..8).map(|i| vec![i as u8; 16]).collect();
        let mut parities = None;
        for pkt in &packets {
            parities = encoder.add_packet(pkt);
        }
        let parities = parities.unwrap();

        // Lose 2 packets from the same half — unrecoverable
        let received: [Option<Vec<u8>>; 8] = [
            None,
            None, // two lost in first half
            Some(packets[2].clone()),
            Some(packets[3].clone()),
            Some(packets[4].clone()),
            Some(packets[5].clone()),
            Some(packets[6].clone()),
            Some(packets[7].clone()),
        ];

        let decoder = FecDecoder::new();
        let result = decoder.decode(&received, &[parities[0].clone(), parities[1].clone()]);

        // Cannot recover — both stay None
        assert!(result[0].is_none());
        assert!(result[1].is_none());
    }
}
