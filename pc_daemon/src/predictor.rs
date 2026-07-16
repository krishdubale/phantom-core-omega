// ============================================================================
// PhantomCore — LSTM-based Speculative Execution Predictor
// Pure Rust implementation (no external ML frameworks)
// Predicts the next 3 likely (func_id, pc) pairs from syscall history
// ============================================================================

use crate::protocol::OffloadRequest;
use anyhow::Result;
use rand::Rng;

/// Dimensionality of the LSTM hidden state
const HIDDEN_SIZE: usize = 64;
/// Input feature size: [func_id_normalized, pc_lower_16, pc_upper_16, arg_hash]
const INPUT_SIZE: usize = 4;
/// Number of output classes (quantized PC buckets)
const OUTPUT_SIZE: usize = 128;
/// How many history entries to keep
const HISTORY_LEN: usize = 32;
/// Number of speculative predictions to generate
pub const PREDICTION_COUNT: usize = 3;

/// Sigmoid activation
fn sigmoid(x: f32) -> f32 {
    1.0 / (1.0 + (-x).exp())
}

/// Tanh activation
fn tanh_activate(x: f32) -> f32 {
    x.tanh()
}

/// Matrix-vector multiply: result = mat * vec (mat is row-major, dims rows x cols)
fn matvec(mat: &[f32], vec: &[f32], rows: usize, cols: usize) -> Vec<f32> {
    let mut result = std::vec![0.0f32; rows];
    for r in 0..rows {
        let mut sum = 0.0f32;
        let row_start = r * cols;
        for c in 0..cols {
            sum += mat[row_start + c] * vec[c];
        }
        result[r] = sum;
    }
    result
}

/// Element-wise vector addition
fn vec_add(a: &[f32], b: &[f32]) -> Vec<f32> {
    a.iter().zip(b.iter()).map(|(x, y)| x + y).collect()
}

/// Element-wise vector multiplication (Hadamard product)
fn vec_mul(a: &[f32], b: &[f32]) -> Vec<f32> {
    a.iter().zip(b.iter()).map(|(x, y)| x * y).collect()
}

/// A single LSTM cell with weights for forget, input, candidate, and output gates
#[derive(Clone, Debug)]
struct LSTMCell {
    // Gate weight matrices: each is HIDDEN_SIZE x (INPUT_SIZE + HIDDEN_SIZE)
    wf: Vec<f32>, // Forget gate weights
    wi: Vec<f32>, // Input gate weights
    wc: Vec<f32>, // Candidate gate weights
    wo: Vec<f32>, // Output gate weights
    // Bias vectors: each is HIDDEN_SIZE
    bf: Vec<f32>,
    bi: Vec<f32>,
    bc: Vec<f32>,
    bo: Vec<f32>,
}

impl LSTMCell {
    /// Initialize with small random weights (Xavier-ish)
    fn random_init() -> Self {
        let mut rng = rand::thread_rng();
        let combined = INPUT_SIZE + HIDDEN_SIZE;
        let scale = (2.0 / combined as f32).sqrt();

        let mut rand_vec = |size: usize| -> Vec<f32> {
            (0..size).map(|_| rng.gen_range(-scale..scale)).collect()
        };

        LSTMCell {
            wf: rand_vec(HIDDEN_SIZE * combined),
            wi: rand_vec(HIDDEN_SIZE * combined),
            wc: rand_vec(HIDDEN_SIZE * combined),
            wo: rand_vec(HIDDEN_SIZE * combined),
            bf: vec![1.0; HIDDEN_SIZE], // Forget gate bias initialized to 1 (keep memory by default)
            bi: vec![0.0; HIDDEN_SIZE],
            bc: vec![0.0; HIDDEN_SIZE],
            bo: vec![0.0; HIDDEN_SIZE],
        }
    }

    /// Load weights from a binary file. Format: all weights concatenated as f32 little-endian.
    fn from_file(path: &str) -> Result<Self> {
        let data = std::fs::read(path)?;
        let floats: Vec<f32> = data
            .chunks_exact(4)
            .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
            .collect();

        let combined = INPUT_SIZE + HIDDEN_SIZE;
        let gate_size = HIDDEN_SIZE * combined;
        let bias_size = HIDDEN_SIZE;
        let expected = 4 * gate_size + 4 * bias_size;

        if floats.len() < expected {
            anyhow::bail!(
                "Weight file too small: got {} floats, need {}",
                floats.len(),
                expected
            );
        }

        let mut offset = 0;
        let take = |o: &mut usize, n: usize| -> Vec<f32> {
            let slice = floats[*o..*o + n].to_vec();
            *o += n;
            slice
        };

        Ok(LSTMCell {
            wf: take(&mut offset, gate_size),
            wi: take(&mut offset, gate_size),
            wc: take(&mut offset, gate_size),
            wo: take(&mut offset, gate_size),
            bf: take(&mut offset, bias_size),
            bi: take(&mut offset, bias_size),
            bc: take(&mut offset, bias_size),
            bo: take(&mut offset, bias_size),
        })
    }

    /// Forward pass through the LSTM cell.
    /// Returns (new_hidden, new_cell_state)
    fn forward(
        &self,
        input: &[f32],
        prev_hidden: &[f32],
        prev_cell: &[f32],
    ) -> (Vec<f32>, Vec<f32>) {
        // Concatenate input and previous hidden state
        let combined_size = INPUT_SIZE + HIDDEN_SIZE;
        let mut combined = Vec::with_capacity(combined_size);
        combined.extend_from_slice(input);
        combined.extend_from_slice(prev_hidden);

        // Forget gate: f = sigmoid(Wf * [input, hidden] + bf)
        let f_raw = vec_add(
            &matvec(&self.wf, &combined, HIDDEN_SIZE, combined_size),
            &self.bf,
        );
        let f: Vec<f32> = f_raw.iter().map(|&x| sigmoid(x)).collect();

        // Input gate: i = sigmoid(Wi * [input, hidden] + bi)
        let i_raw = vec_add(
            &matvec(&self.wi, &combined, HIDDEN_SIZE, combined_size),
            &self.bi,
        );
        let i: Vec<f32> = i_raw.iter().map(|&x| sigmoid(x)).collect();

        // Candidate: c_hat = tanh(Wc * [input, hidden] + bc)
        let c_raw = vec_add(
            &matvec(&self.wc, &combined, HIDDEN_SIZE, combined_size),
            &self.bc,
        );
        let c_hat: Vec<f32> = c_raw.iter().map(|&x| tanh_activate(x)).collect();

        // Output gate: o = sigmoid(Wo * [input, hidden] + bo)
        let o_raw = vec_add(
            &matvec(&self.wo, &combined, HIDDEN_SIZE, combined_size),
            &self.bo,
        );
        let o: Vec<f32> = o_raw.iter().map(|&x| sigmoid(x)).collect();

        // New cell state: cell = f * prev_cell + i * c_hat
        let new_cell = vec_add(&vec_mul(&f, prev_cell), &vec_mul(&i, &c_hat));

        // New hidden state: hidden = o * tanh(cell)
        let cell_tanh: Vec<f32> = new_cell.iter().map(|&x| tanh_activate(x)).collect();
        let new_hidden = vec_mul(&o, &cell_tanh);

        (new_hidden, new_cell)
    }
}

/// Output projection layer: maps hidden state to output logits
#[derive(Clone, Debug)]
struct OutputLayer {
    weights: Vec<f32>, // OUTPUT_SIZE x HIDDEN_SIZE
    bias: Vec<f32>,    // OUTPUT_SIZE
}

impl OutputLayer {
    fn random_init() -> Self {
        let mut rng = rand::thread_rng();
        let scale = (2.0 / HIDDEN_SIZE as f32).sqrt();
        OutputLayer {
            weights: (0..OUTPUT_SIZE * HIDDEN_SIZE)
                .map(|_| rng.gen_range(-scale..scale))
                .collect(),
            bias: vec![0.0; OUTPUT_SIZE],
        }
    }

    /// Project hidden state to output logits
    fn forward(&self, hidden: &[f32]) -> Vec<f32> {
        vec_add(
            &matvec(&self.weights, hidden, OUTPUT_SIZE, HIDDEN_SIZE),
            &self.bias,
        )
    }
}

/// Top-level LSTM predictor that wraps the cell, output layer, and history
#[derive(Clone, Debug)]
pub struct LSTMPredictor {
    cell: LSTMCell,
    output: OutputLayer,
    hidden: Vec<f32>,
    cell_state: Vec<f32>,
    /// Rolling history of (func_id, pc) pairs
    history: Vec<(u32, u64)>,
    /// Mapping from output class index to (func_id, pc) pairs seen in training
    class_to_target: Vec<(u32, u64)>,
}

impl LSTMPredictor {
    /// Load predictor weights from a file, or initialize randomly if file not found
    pub fn load(path: &str) -> Result<Self> {
        let cell = match LSTMCell::from_file(path) {
            Ok(c) => {
                log::info!("Loaded LSTM weights from {}", path);
                c
            }
            Err(_) => {
                log::warn!(
                    "Could not load weights from '{}', using random initialization",
                    path
                );
                LSTMCell::random_init()
            }
        };

        // Initialize the class-to-target mapping with common syscall patterns
        let mut class_to_target = Vec::with_capacity(OUTPUT_SIZE);
        for i in 0..OUTPUT_SIZE {
            // Default mapping: spread func_ids across classes
            let func_id = match i % 4 {
                0 => 64,  // write
                1 => 29,  // ioctl
                2 => 98,  // futex
                _ => 0,   // unknown
            };
            class_to_target.push((func_id, (i as u64) * 0x100));
        }

        Ok(LSTMPredictor {
            cell,
            output: OutputLayer::random_init(),
            hidden: vec![0.0; HIDDEN_SIZE],
            cell_state: vec![0.0; HIDDEN_SIZE],
            history: Vec::with_capacity(HISTORY_LEN),
            class_to_target,
        })
    }

    /// Convert an OffloadRequest into a fixed-size input feature vector
    fn extract_features(req: &OffloadRequest) -> [f32; INPUT_SIZE] {
        [
            req.func_id as f32 / 256.0,                   // Normalized syscall ID
            (req.pc & 0xFFFF) as f32 / 65536.0,           // Lower 16 bits of PC
            ((req.pc >> 16) & 0xFFFF) as f32 / 65536.0,   // Upper 16 bits of PC
            (req.registers[0] % 256) as f32 / 256.0,      // Hash of first arg
        ]
    }

    /// Feed an observed request through the LSTM and predict the next 3 likely calls
    pub fn predict_next(&mut self, req: &OffloadRequest) -> Vec<OffloadRequest> {
        // Record in history
        self.history.push((req.func_id, req.pc));
        if self.history.len() > HISTORY_LEN {
            self.history.remove(0);
        }

        // Update class mapping with observed (func_id, pc) pairs
        let class_idx = (req.pc as usize) % OUTPUT_SIZE;
        self.class_to_target[class_idx] = (req.func_id, req.pc);

        // Run LSTM forward pass
        let features = Self::extract_features(req);
        let (new_hidden, new_cell) =
            self.cell
                .forward(&features, &self.hidden, &self.cell_state);
        self.hidden = new_hidden;
        self.cell_state = new_cell;

        // Get output logits
        let logits = self.output.forward(&self.hidden);

        // Softmax and pick top-3
        let max_logit = logits.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
        let exp_logits: Vec<f32> = logits.iter().map(|&x| (x - max_logit).exp()).collect();
        let sum_exp: f32 = exp_logits.iter().sum();
        let probs: Vec<f32> = exp_logits.iter().map(|&x| x / sum_exp).collect();

        // Find top 3 indices
        let mut indexed: Vec<(usize, f32)> = probs.iter().cloned().enumerate().collect();
        indexed.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        let mut predictions = Vec::with_capacity(PREDICTION_COUNT);
        for i in 0..PREDICTION_COUNT.min(indexed.len()) {
            let (class_idx, _prob) = indexed[i];
            let (pred_func_id, pred_pc) = self.class_to_target[class_idx];

            predictions.push(OffloadRequest {
                session_id: req.session_id,
                seq: req.seq + (i as u32) + 1, // Predicted future sequence numbers
                func_id: pred_func_id,
                flags: crate::protocol::FLAG_SPECULATIVE,
                payload: Vec::new(),
                registers: req.registers, // Assume same register state
                pc: pred_pc,
            });
        }

        predictions
    }

    /// Reset internal state (e.g., on new session)
    #[allow(dead_code)]
    pub fn reset(&mut self) {
        self.hidden = vec![0.0; HIDDEN_SIZE];
        self.cell_state = vec![0.0; HIDDEN_SIZE];
        self.history.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_predictor_produces_3_predictions() {
        let mut predictor = LSTMPredictor::load("nonexistent.weights").unwrap();
        let req = OffloadRequest {
            session_id: 1,
            seq: 100,
            func_id: 64,
            flags: 0,
            payload: vec![],
            registers: [0; 31],
            pc: 0x4000,
        };
        let preds = predictor.predict_next(&req);
        assert_eq!(preds.len(), PREDICTION_COUNT);
        // Verify predicted seq numbers are sequential after the request
        assert_eq!(preds[0].seq, 101);
        assert_eq!(preds[1].seq, 102);
        assert_eq!(preds[2].seq, 103);
    }

    #[test]
    fn test_predictor_state_updates() {
        let mut predictor = LSTMPredictor::load("nonexistent.weights").unwrap();
        let initial_hidden = predictor.hidden.clone();

        let req = OffloadRequest {
            session_id: 1,
            seq: 1,
            func_id: 29,
            flags: 0,
            payload: vec![],
            registers: [42; 31],
            pc: 0x8000,
        };
        predictor.predict_next(&req);

        // Hidden state should have changed
        assert_ne!(predictor.hidden, initial_hidden);
        assert_eq!(predictor.history.len(), 1);
    }

    #[test]
    fn test_sigmoid() {
        assert!((sigmoid(0.0) - 0.5).abs() < 1e-6);
        assert!(sigmoid(100.0) > 0.999);
        assert!(sigmoid(-100.0) < 0.001);
    }

    #[test]
    fn test_matvec() {
        // 2x3 matrix times 3-vector
        let mat = vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0];
        let vec_in = vec![1.0, 1.0, 1.0];
        let result = matvec(&mat, &vec_in, 2, 3);
        assert_eq!(result, vec![6.0, 15.0]);
    }
}
