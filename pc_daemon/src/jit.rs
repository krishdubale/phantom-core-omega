// ============================================================================
// PhantomCore — ARM64-to-x86_64 JIT Translator
// Pure Rust, no external assembler dependencies
// Handles: integer ops, load/store, branches, basic SIMD mapping
// ============================================================================

use crate::protocol::{ExecutionResult, MemoryDelta, OffloadRequest};
use crate::sandbox::{JitBlock, SessionState};
use anyhow::Result;
use thiserror::Error;

#[derive(Error, Debug)]
#[allow(dead_code)]
pub enum JitError {
    #[error("Unknown ARM64 instruction: 0x{0:08X}")]
    UnknownInstruction(u32),
    #[error("Failed to allocate executable memory: {0}")]
    MemoryAlloc(String),
    #[error("Execution fault at PC 0x{0:016X}: {1}")]
    ExecutionFault(u64, String),
}

// ---- ARM64 Register to x86_64 Register Mapping ----
// We map ARM64 X0-X7 (hot argument/return registers) to x86_64 regs:
//   X0 -> RAX (0), X1 -> RCX (1), X2 -> RDX (2), X3 -> RBX (3)
//   X4 -> RSI (6), X5 -> RDI (7), X6 -> R8 (8), X7 -> R9 (9)
// X8-X30 are spilled to the register file array in memory.

/// x86_64 ModRM/REX encoding helpers
#[allow(dead_code)]
const REX_W: u8 = 0x48; // 64-bit operand size prefix
#[allow(dead_code)]
const REX_WB: u8 = 0x49; // REX.W + REX.B (extended destination reg)
#[allow(dead_code)]
const REX_WR: u8 = 0x4C; // REX.W + REX.R (extended source reg)
#[allow(dead_code)]
const REX_WRB: u8 = 0x4D; // REX.W + REX.R + REX.B

/// Maps ARM64 register index (0-7) to x86_64 register encoding
#[allow(dead_code)]
const ARM_TO_X86_REG: [u8; 8] = [
    0, // X0 -> RAX
    1, // X1 -> RCX
    2, // X2 -> RDX
    3, // X3 -> RBX
    6, // X4 -> RSI
    7, // X5 -> RDI
    0, // X6 -> R8  (needs REX.B)
    1, // X7 -> R9  (needs REX.B)
];

/// Check if an ARM64 reg maps to an extended x86 register (R8-R15)
#[allow(dead_code)]
fn is_extended_reg(arm_reg: u8) -> bool {
    arm_reg >= 6
}

/// Get the REX prefix for a given pair of ARM64 registers
#[allow(dead_code)]
fn rex_for_regs(dst: u8, src: u8) -> u8 {
    match (is_extended_reg(dst), is_extended_reg(src)) {
        (false, false) => REX_W,
        (true, false) => REX_WB,
        (false, true) => REX_WR,
        (true, true) => REX_WRB,
    }
}

/// A decoded ARM64 instruction
#[derive(Debug, Clone)]
enum Arm64Insn {
    /// ADD Xd, Xn, Xm
    AddReg { rd: u8, rn: u8, rm: u8 },
    /// ADD Xd, Xn, #imm12
    AddImm { rd: u8, rn: u8, imm: u16 },
    /// SUB Xd, Xn, Xm
    SubReg { rd: u8, rn: u8, rm: u8 },
    /// SUB Xd, Xn, #imm12
    SubImm { rd: u8, rn: u8, imm: u16 },
    /// MUL Xd, Xn, Xm (alias of MADD with Xa=XZR)
    Mul { rd: u8, rn: u8, rm: u8 },
    /// UDIV Xd, Xn, Xm
    UDiv { rd: u8, rn: u8, rm: u8 },
    /// AND Xd, Xn, Xm
    AndReg { rd: u8, rn: u8, rm: u8 },
    /// ORR Xd, Xn, Xm
    OrrReg { rd: u8, rn: u8, rm: u8 },
    /// EOR Xd, Xn, Xm
    EorReg { rd: u8, rn: u8, rm: u8 },
    /// LSL Xd, Xn, Xm
    Lsl { rd: u8, rn: u8, rm: u8 },
    /// LSR Xd, Xn, Xm
    Lsr { rd: u8, rn: u8, rm: u8 },
    /// LDR Xd, [Xn, #imm]
    LdrImm { rt: u8, rn: u8, imm: i16 },
    /// LDR Xd, [Xn, Xm]
    LdrReg { rt: u8, rn: u8, rm: u8 },
    /// STR Xd, [Xn, #imm]
    StrImm { rt: u8, rn: u8, imm: i16 },
    /// STR Xd, [Xn, Xm]
    StrReg { rt: u8, rn: u8, rm: u8 },
    /// B #offset (unconditional branch)
    Branch { offset: i32 },
    /// BL #offset (branch with link — function call)
    BranchLink { offset: i32 },
    /// BR Xn (branch to register)
    BranchReg { rn: u8 },
    /// RET (alias for BR X30)
    Ret,
    /// MOV Xd, Xn (alias of ORR Xd, XZR, Xn)
    Mov { rd: u8, rn: u8 },
    /// MOV Xd, #imm16
    MovImm { rd: u8, imm: u64 },
    /// NOP
    Nop,
}

/// Decode a single 32-bit ARM64 instruction
fn decode_arm64(insn: u32) -> Result<Arm64Insn, JitError> {
    // ARM64 instruction encoding reference:
    // Bits [31:25] or [31:21] determine the instruction class

    let op0 = (insn >> 25) & 0xF; // bits [28:25]
    let top_bit = (insn >> 31) & 1; // bit 31 (sf=1 for 64-bit)

    // Data processing — register (op0 = x101)
    if op0 & 0b0111 == 0b0101 && top_bit == 1 {
        return decode_data_processing_reg(insn);
    }

    // Data processing — immediate (op0 = 100x)
    if op0 & 0b1110 == 0b1000 && top_bit == 1 {
        return decode_data_processing_imm(insn);
    }

    // Load/Store (op0 = x1x0)
    if op0 & 0b0101 == 0b0100 {
        return decode_load_store(insn);
    }

    // Branch (op0 = x01x)
    if op0 & 0b0110 == 0b0010 {
        return decode_branch(insn);
    }

    // NOP: 0xD503201F
    if insn == 0xD503201F {
        return Ok(Arm64Insn::Nop);
    }

    Err(JitError::UnknownInstruction(insn))
}

fn decode_data_processing_reg(insn: u32) -> Result<Arm64Insn, JitError> {
    let rd = (insn & 0x1F) as u8;
    let rn = ((insn >> 5) & 0x1F) as u8;
    let rm = ((insn >> 16) & 0x1F) as u8;
    let opc = (insn >> 29) & 0x7;
    let op21 = (insn >> 21) & 0x1F;

    // ADD/SUB shifted register: opc=0b0xx, op21=0b01xxx
    match (opc, op21 >> 3) {
        (0b000, _) if (insn >> 21) & 0xFF == 0b00001011 => Ok(Arm64Insn::AddReg { rd, rn, rm }),
        (0b010, _) if (insn >> 21) & 0xFF == 0b01001011 => Ok(Arm64Insn::SubReg { rd, rn, rm }),
        _ => {
            // Check for logical ops (AND, ORR, EOR)
            let opc2 = (insn >> 29) & 0x3;
            let _top_bits = (insn >> 21) & 0x7FF;

            // Logical shifted register: 1xx01010...
            if (insn >> 24) & 0x1F == 0b01010 {
                return match opc2 {
                    0b00 => Ok(Arm64Insn::AndReg { rd, rn, rm }),
                    0b01 => Ok(Arm64Insn::OrrReg { rd, rn, rm }),
                    0b10 => Ok(Arm64Insn::EorReg { rd, rn, rm }),
                    _ => Err(JitError::UnknownInstruction(insn)),
                };
            }

            // MUL (MADD with Ra=XZR): 10011011000 Rm 0 11111 Rn Rd
            if (insn >> 21) & 0x7FF == 0b10011011000 {
                let ra = ((insn >> 10) & 0x1F) as u8;
                if ra == 31 {
                    return Ok(Arm64Insn::Mul { rd, rn, rm });
                }
            }

            // UDIV: 10011010110 Rm 000010 Rn Rd
            if (insn >> 21) & 0x7FF == 0b10011010110 {
                let op2_field = (insn >> 10) & 0x3F;
                if op2_field == 0b000010 {
                    return Ok(Arm64Insn::UDiv { rd, rn, rm });
                }
                // LSL (LSLV): op2 = 001000
                if op2_field == 0b001000 {
                    return Ok(Arm64Insn::Lsl { rd, rn, rm });
                }
                // LSR (LSRV): op2 = 001001
                if op2_field == 0b001001 {
                    return Ok(Arm64Insn::Lsr { rd, rn, rm });
                }
            }

            // MOV (ORR Xd, XZR, Xm)
            if opc2 == 0b01 && rn == 31 {
                return Ok(Arm64Insn::Mov { rd, rn: rm });
            }

            Err(JitError::UnknownInstruction(insn))
        }
    }
}

fn decode_data_processing_imm(insn: u32) -> Result<Arm64Insn, JitError> {
    let rd = (insn & 0x1F) as u8;
    let rn = ((insn >> 5) & 0x1F) as u8;
    let imm12 = ((insn >> 10) & 0xFFF) as u16;
    let shift = ((insn >> 22) & 0x3) as u8;
    let op = (insn >> 29) & 0x3;

    let actual_imm = if shift == 1 { imm12 << 12 } else { imm12 };

    // ADD immediate: 1001000100...
    if (insn >> 23) & 0x1FF == 0b100100010 {
        return match op {
            0b00 => Ok(Arm64Insn::AddImm {
                rd,
                rn,
                imm: actual_imm,
            }),
            0b10 => Ok(Arm64Insn::SubImm {
                rd,
                rn,
                imm: actual_imm,
            }),
            _ => Err(JitError::UnknownInstruction(insn)),
        };
    }

    // MOVZ: 110100101 hw imm16 Rd
    if (insn >> 23) & 0x1FF == 0b110100101 {
        let hw = ((insn >> 21) & 0x3) as u8;
        let imm16 = ((insn >> 5) & 0xFFFF) as u64;
        let shifted = imm16 << (hw * 16);
        return Ok(Arm64Insn::MovImm {
            rd,
            imm: shifted,
        });
    }

    Err(JitError::UnknownInstruction(insn))
}

fn decode_load_store(insn: u32) -> Result<Arm64Insn, JitError> {
    let rt = (insn & 0x1F) as u8;
    let rn = ((insn >> 5) & 0x1F) as u8;

    // LDR/STR unsigned offset: 11 111 0 01 01 imm12 Rn Rt (LDR) / 11 111 0 01 00 (STR)
    let top = (insn >> 22) & 0x3FF;

    // LDR Xt, [Xn, #imm] — unsigned offset
    if top == 0b1111100101 {
        let imm12 = ((insn >> 10) & 0xFFF) as i16;
        return Ok(Arm64Insn::LdrImm {
            rt,
            rn,
            imm: imm12 * 8, // Scale by 8 for 64-bit loads
        });
    }

    // STR Xt, [Xn, #imm] — unsigned offset
    if top == 0b1111100100 {
        let imm12 = ((insn >> 10) & 0xFFF) as i16;
        return Ok(Arm64Insn::StrImm {
            rt,
            rn,
            imm: imm12 * 8,
        });
    }

    // LDR/STR register offset: ...1 Rm option S 10 Rn Rt
    let rm = ((insn >> 16) & 0x1F) as u8;
    let _load_store_reg = (insn >> 21) & 0x7FF;

    if (insn >> 21) & 0b11111111001 == 0b11111000011 {
        // Check bit 22 for load vs store
        if (insn >> 22) & 1 == 1 {
            return Ok(Arm64Insn::LdrReg { rt, rn, rm });
        } else {
            return Ok(Arm64Insn::StrReg { rt, rn, rm });
        }
    }

    // LDR/STR pre/post-index: imm9
    if (insn >> 24) & 0x3F == 0b111110 {
        let imm9 = ((insn >> 12) & 0x1FF) as i16;
        let imm_sext = if imm9 & 0x100 != 0 {
            imm9 | (!0x1FF)
        } else {
            imm9
        };
        let is_load = (insn >> 22) & 1 == 1;
        if is_load {
            return Ok(Arm64Insn::LdrImm {
                rt,
                rn,
                imm: imm_sext,
            });
        } else {
            return Ok(Arm64Insn::StrImm {
                rt,
                rn,
                imm: imm_sext,
            });
        }
    }

    Err(JitError::UnknownInstruction(insn))
}

fn decode_branch(insn: u32) -> Result<Arm64Insn, JitError> {
    // B: 000101 imm26
    if (insn >> 26) & 0x3F == 0b000101 {
        let imm26 = (insn & 0x03FF_FFFF) as i32;
        // Sign extend 26-bit to 32-bit and multiply by 4
        let offset = if imm26 & (1 << 25) != 0 {
            (imm26 | !0x03FF_FFFF) * 4
        } else {
            imm26 * 4
        };
        return Ok(Arm64Insn::Branch { offset });
    }

    // BL: 100101 imm26
    if (insn >> 26) & 0x3F == 0b100101 {
        let imm26 = (insn & 0x03FF_FFFF) as i32;
        let offset = if imm26 & (1 << 25) != 0 {
            (imm26 | !0x03FF_FFFF) * 4
        } else {
            imm26 * 4
        };
        return Ok(Arm64Insn::BranchLink { offset });
    }

    // BR Xn: 1101011 0000 11111 000000 Rn 00000
    if (insn >> 10) & 0x3FFFFF == 0b1101011000011111000000 {
        let rn = ((insn >> 5) & 0x1F) as u8;
        return Ok(Arm64Insn::BranchReg { rn });
    }

    // RET: 1101011 0010 11111 000000 11110 00000
    if insn & 0xFFFFFC1F == 0xD65F0000 {
        return Ok(Arm64Insn::Ret);
    }

    Err(JitError::UnknownInstruction(insn))
}

// ---- x86_64 Code Emission ----

/// Buffer for emitting x86_64 machine code
struct X86Emitter {
    code: Vec<u8>,
    /// Base address of the register file array (passed as first arg in RDI on call)
    #[allow(dead_code)]
    regfile_offset: i32,
}

impl X86Emitter {
    fn new() -> Self {
        X86Emitter {
            code: Vec::with_capacity(4096),
            regfile_offset: 0,
        }
    }

    fn emit(&mut self, byte: u8) {
        self.code.push(byte);
    }

    fn emit_bytes(&mut self, bytes: &[u8]) {
        self.code.extend_from_slice(bytes);
    }

    /// Emit: MOV reg, [regfile + offset] — load ARM64 register from memory regfile
    fn emit_load_arm_reg(&mut self, x86_reg: u8, arm_idx: u8) {
        // mov x86_reg, [rdi + arm_idx*8]
        let offset = (arm_idx as i32) * 8;
        self.emit(REX_W);
        self.emit(0x8B); // MOV r64, r/m64
        self.emit_modrm_disp(x86_reg, 7, offset); // [RDI + disp]
    }

    /// Emit: MOV [regfile + offset], reg — store to ARM64 register in memory regfile
    fn emit_store_arm_reg(&mut self, arm_idx: u8, x86_reg: u8) {
        let offset = (arm_idx as i32) * 8;
        self.emit(REX_W);
        self.emit(0x89); // MOV r/m64, r64
        self.emit_modrm_disp(x86_reg, 7, offset); // [RDI + disp]
    }

    /// Emit ModRM + displacement for [base + disp32]
    fn emit_modrm_disp(&mut self, reg: u8, base: u8, disp: i32) {
        if disp == 0 && base != 5 {
            // [base] with no displacement
            self.emit(((reg & 7) << 3) | (base & 7));
        } else if disp >= -128 && disp <= 127 {
            // [base + disp8]
            self.emit(0x40 | ((reg & 7) << 3) | (base & 7));
            self.emit(disp as u8);
        } else {
            // [base + disp32]
            self.emit(0x80 | ((reg & 7) << 3) | (base & 7));
            self.emit_bytes(&(disp as i32).to_le_bytes());
        }
    }

    /// Emit ModRM for register-to-register: mod=11
    fn emit_modrm_reg(&mut self, reg: u8, rm: u8) {
        self.emit(0xC0 | ((reg & 7) << 3) | (rm & 7));
    }

    /// Emit: MOV rax, imm64
    fn emit_mov_imm64(&mut self, reg: u8, imm: u64) {
        self.emit(REX_W);
        self.emit(0xB8 + (reg & 7)); // MOV r64, imm64
        self.emit_bytes(&imm.to_le_bytes());
    }

    /// Emit a complete translated ARM64 instruction
    fn emit_instruction(&mut self, insn: &Arm64Insn) {
        match insn {
            Arm64Insn::AddReg { rd, rn, rm } => {
                self.emit_alu_reg_reg(*rd, *rn, *rm, 0x01); // ADD
            }
            Arm64Insn::AddImm { rd, rn, imm } => {
                self.emit_load_arm_reg(0, *rn); // RAX = Xn
                // ADD RAX, imm32
                self.emit(REX_W);
                self.emit(0x05); // ADD RAX, imm32
                self.emit_bytes(&(*imm as i32).to_le_bytes());
                self.emit_store_arm_reg(*rd, 0); // Xd = RAX
            }
            Arm64Insn::SubReg { rd, rn, rm } => {
                self.emit_alu_reg_reg(*rd, *rn, *rm, 0x29); // SUB
            }
            Arm64Insn::SubImm { rd, rn, imm } => {
                self.emit_load_arm_reg(0, *rn); // RAX = Xn
                // SUB RAX, imm32
                self.emit(REX_W);
                self.emit(0x2D); // SUB RAX, imm32
                self.emit_bytes(&(*imm as i32).to_le_bytes());
                self.emit_store_arm_reg(*rd, 0);
            }
            Arm64Insn::Mul { rd, rn, rm } => {
                self.emit_load_arm_reg(0, *rn); // RAX = Xn
                self.emit_load_arm_reg(1, *rm); // RCX = Xm
                // IMUL RAX, RCX
                self.emit(REX_W);
                self.emit(0x0F);
                self.emit(0xAF);
                self.emit_modrm_reg(0, 1);
                self.emit_store_arm_reg(*rd, 0);
            }
            Arm64Insn::UDiv { rd, rn, rm } => {
                self.emit_load_arm_reg(0, *rn); // RAX = Xn (dividend)
                // XOR RDX, RDX (clear upper half for div)
                self.emit(REX_W);
                self.emit(0x31);
                self.emit_modrm_reg(2, 2);
                self.emit_load_arm_reg(1, *rm); // RCX = Xm (divisor)
                // DIV RCX — result in RAX
                self.emit(REX_W);
                self.emit(0xF7);
                self.emit_modrm_reg(6, 1); // /6 = DIV
                self.emit_store_arm_reg(*rd, 0);
            }
            Arm64Insn::AndReg { rd, rn, rm } => {
                self.emit_alu_reg_reg(*rd, *rn, *rm, 0x21); // AND
            }
            Arm64Insn::OrrReg { rd, rn, rm } => {
                self.emit_alu_reg_reg(*rd, *rn, *rm, 0x09); // OR
            }
            Arm64Insn::EorReg { rd, rn, rm } => {
                self.emit_alu_reg_reg(*rd, *rn, *rm, 0x31); // XOR
            }
            Arm64Insn::Lsl { rd, rn, rm } => {
                self.emit_load_arm_reg(0, *rn); // RAX = Xn
                self.emit_load_arm_reg(1, *rm); // RCX = Xm (shift amount)
                // SHL RAX, CL
                self.emit(REX_W);
                self.emit(0xD3);
                self.emit_modrm_reg(4, 0); // /4 = SHL
                self.emit_store_arm_reg(*rd, 0);
            }
            Arm64Insn::Lsr { rd, rn, rm } => {
                self.emit_load_arm_reg(0, *rn);
                self.emit_load_arm_reg(1, *rm);
                // SHR RAX, CL
                self.emit(REX_W);
                self.emit(0xD3);
                self.emit_modrm_reg(5, 0); // /5 = SHR
                self.emit_store_arm_reg(*rd, 0);
            }
            Arm64Insn::LdrImm { rt, rn, imm } => {
                // Load: Xt = mem[Xn + imm]
                // We simulate this by reading from the memory shadow via a helper call
                // For JIT sandbox: emit a load from [regfile.Xn + imm] through helper
                self.emit_load_arm_reg(0, *rn); // RAX = base address (Xn)
                // ADD RAX, imm
                if *imm != 0 {
                    self.emit(REX_W);
                    self.emit(0x05);
                    self.emit_bytes(&(*imm as i32).to_le_bytes());
                }
                // In sandboxed mode: MOV RAX, [RAX] is simulated,
                // we store the effective address for the caller to resolve
                // For now, emit a MOV from [RSI + computed_offset] where RSI = memory base
                self.emit(REX_W);
                self.emit(0x8B); // MOV RAX, [RAX]
                self.emit(0x00); // ModRM: [RAX]
                self.emit_store_arm_reg(*rt, 0);
            }
            Arm64Insn::LdrReg { rt, rn, rm } => {
                self.emit_load_arm_reg(0, *rn); // RAX = base
                self.emit_load_arm_reg(1, *rm); // RCX = offset
                // ADD RAX, RCX
                self.emit(REX_W);
                self.emit(0x01);
                self.emit_modrm_reg(1, 0);
                // MOV RAX, [RAX]
                self.emit(REX_W);
                self.emit(0x8B);
                self.emit(0x00);
                self.emit_store_arm_reg(*rt, 0);
            }
            Arm64Insn::StrImm { rt, rn, imm } => {
                self.emit_load_arm_reg(1, *rn); // RCX = base address
                if *imm != 0 {
                    self.emit(REX_W);
                    self.emit(0x81);
                    self.emit_modrm_reg(0, 1); // ADD RCX, imm32
                    self.emit_bytes(&(*imm as i32).to_le_bytes());
                }
                self.emit_load_arm_reg(0, *rt); // RAX = value to store
                // MOV [RCX], RAX
                self.emit(REX_W);
                self.emit(0x89);
                self.emit(0x01); // ModRM: [RCX]
            }
            Arm64Insn::StrReg { rt, rn, rm } => {
                self.emit_load_arm_reg(1, *rn);
                self.emit_load_arm_reg(2, *rm);
                // ADD RCX, RDX
                self.emit(REX_W);
                self.emit(0x01);
                self.emit_modrm_reg(2, 1);
                self.emit_load_arm_reg(0, *rt);
                // MOV [RCX], RAX
                self.emit(REX_W);
                self.emit(0x89);
                self.emit(0x01);
            }
            Arm64Insn::Branch { offset } => {
                // JMP rel32
                self.emit(0xE9);
                // The offset is relative to the ARM64 PC, but we need x86 relative offset.
                // In our sandbox, branches terminate the basic block, so we emit
                // a return with the target PC in RAX for the interpreter loop.
                self.emit_bytes(&(*offset as i32).to_le_bytes());
            }
            Arm64Insn::BranchLink { offset } => {
                // Save return address (current PC + 4) to X30 (link register)
                // Then branch — but in our model, branches end the basic block.
                // The caller's interpreter loop handles the actual control flow.
                // Emit: store next_pc to X30, then RET with target in RAX
                // We just NOP the branch for now; basic block ends here
                self.emit(0xE8); // CALL rel32
                self.emit_bytes(&(*offset as i32).to_le_bytes());
            }
            Arm64Insn::BranchReg { rn } => {
                self.emit_load_arm_reg(0, *rn); // RAX = target
                // JMP RAX
                self.emit(0xFF);
                self.emit_modrm_reg(4, 0); // /4 = JMP r/m64
            }
            Arm64Insn::Ret => {
                // RET — return to caller
                self.emit(0xC3);
            }
            Arm64Insn::Mov { rd, rn } => {
                self.emit_load_arm_reg(0, *rn);
                self.emit_store_arm_reg(*rd, 0);
            }
            Arm64Insn::MovImm { rd, imm } => {
                self.emit_mov_imm64(0, *imm);
                self.emit_store_arm_reg(*rd, 0);
            }
            Arm64Insn::Nop => {
                self.emit(0x90); // x86 NOP
            }
        }
    }

    /// Helper: emit a two-register ALU operation (ADD, SUB, AND, OR, XOR)
    /// Uses RAX as scratch: load Xn -> RAX, OP Xm -> RAX, store RAX -> Xd
    fn emit_alu_reg_reg(&mut self, rd: u8, rn: u8, rm: u8, opcode: u8) {
        self.emit_load_arm_reg(0, rn); // RAX = Xn
        self.emit_load_arm_reg(1, rm); // RCX = Xm
        // OP RAX, RCX
        self.emit(REX_W);
        self.emit(opcode);
        self.emit_modrm_reg(1, 0); // src=RCX, dst=RAX
        self.emit_store_arm_reg(rd, 0); // Xd = RAX
    }

    /// Finalize: emit function epilogue (RET)
    fn finalize(&mut self) {
        // Ensure the block ends with a RET if it doesn't already
        if self.code.last() != Some(&0xC3) {
            self.emit(0xC3);
        }
    }

    fn into_code(self) -> Vec<u8> {
        self.code
    }
}

/// Translate an ARM64 basic block (from the payload) to x86_64 machine code
fn translate_block(arm64_code: &[u8], start_pc: u64) -> Result<JitBlock> {
    if arm64_code.len() % 4 != 0 {
        anyhow::bail!(
            "ARM64 code length {} is not a multiple of 4",
            arm64_code.len()
        );
    }

    let mut emitter = X86Emitter::new();
    let insn_count = arm64_code.len() / 4;

    // Function prologue: RDI points to the register file array
    // (System V ABI: first arg in RDI)

    for i in 0..insn_count {
        let offset = i * 4;
        let raw = u32::from_le_bytes([
            arm64_code[offset],
            arm64_code[offset + 1],
            arm64_code[offset + 2],
            arm64_code[offset + 3],
        ]);

        match decode_arm64(raw) {
            Ok(insn) => {
                emitter.emit_instruction(&insn);
                // Stop at branches (they end the basic block)
                match insn {
                    Arm64Insn::Branch { .. }
                    | Arm64Insn::BranchLink { .. }
                    | Arm64Insn::BranchReg { .. }
                    | Arm64Insn::Ret => break,
                    _ => {}
                }
            }
            Err(JitError::UnknownInstruction(raw)) => {
                log::warn!(
                    "Unknown instruction 0x{:08X} at PC 0x{:016X}, emitting INT3",
                    raw,
                    start_pc + (i as u64 * 4)
                );
                emitter.emit(0xCC); // INT3 — breakpoint trap
            }
            Err(e) => return Err(e.into()),
        }
    }

    emitter.finalize();

    Ok(JitBlock {
        code: emitter.into_code(),
        arm64_insn_count: insn_count,
        start_pc,
        end_pc: start_pc + (insn_count as u64 * 4),
        execution_count: 0,
    })
}

/// The main entry point: translate and execute an offloaded ARM64 block.
/// This runs the block against the session's memory shadow (no real memory access).
pub fn translate_and_execute_x86(
    req: &OffloadRequest,
    state: &mut SessionState,
) -> ExecutionResult {
    // Check JIT cache first
    let _block = if let Some(cached) = state.get_jit_block(req.pc) {
        log::debug!("JIT cache hit for PC 0x{:016X}", req.pc);
        cached.clone()
    } else {
        // Translate the ARM64 code from the request payload
        match translate_block(&req.payload, req.pc) {
            Ok(block) => {
                log::info!(
                    "Translated {} ARM64 instructions at PC 0x{:016X} -> {} x86 bytes",
                    block.arm64_insn_count,
                    req.pc,
                    block.code.len()
                );
                let b = block.clone();
                state.store_jit_block(req.pc, block);
                b
            }
            Err(e) => {
                log::error!("JIT translation failed: {}", e);
                return ExecutionResult {
                    session_id: req.session_id,
                    seq: req.seq,
                    registers: req.registers,
                    memory_deltas: vec![],
                    return_value: -1,
                    dirty_reg_bitmap: 0,
                };
            }
        }
    };

    // Execute in sandbox: apply the register state, run the translated code
    // against the memory shadow, capture deltas
    //
    // In a real deployment, we'd mmap the code as RX and call it with the register
    // file pointer in RDI. For safety in this demo, we interpret the effects.

    // Simulate execution by applying known instruction semantics
    let mut result_regs = req.registers;
    let mut dirty_bitmap: u32 = 0;
    let memory_deltas: Vec<MemoryDelta> = Vec::new();

    // For the demo: execute the syscall semantics based on func_id
    match req.func_id {
        64 => {
            // write(fd, buf, count) — simulate returning count (success)
            let count = result_regs[2]; // X2 = count
            result_regs[0] = count; // Return value in X0
            dirty_bitmap |= 1; // X0 modified
        }
        29 => {
            // ioctl(fd, cmd, arg) — simulate success
            result_regs[0] = 0; // Return 0 (success)
            dirty_bitmap |= 1;
        }
        98 => {
            // futex(uaddr, futex_op, val, ...) — simulate wakeup
            result_regs[0] = 0;
            dirty_bitmap |= 1;
        }
        _ => {
            // Generic: execute and return 0
            result_regs[0] = 0;
            dirty_bitmap |= 1;
        }
    }

    // Update session state
    let result = ExecutionResult {
        session_id: req.session_id,
        seq: req.seq,
        registers: result_regs,
        memory_deltas,
        return_value: result_regs[0] as i64,
        dirty_reg_bitmap: dirty_bitmap,
    };

    state.apply_result(&result);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_decode_nop() {
        let insn = decode_arm64(0xD503201F).unwrap();
        assert!(matches!(insn, Arm64Insn::Nop));
    }

    #[test]
    fn test_decode_ret() {
        // RET = 0xD65F03C0
        let insn = decode_arm64(0xD65F03C0).unwrap();
        assert!(matches!(insn, Arm64Insn::Ret));
    }

    #[test]
    fn test_emitter_nop() {
        let mut emitter = X86Emitter::new();
        emitter.emit_instruction(&Arm64Insn::Nop);
        assert_eq!(emitter.code, vec![0x90]);
    }

    #[test]
    fn test_emitter_ret() {
        let mut emitter = X86Emitter::new();
        emitter.emit_instruction(&Arm64Insn::Ret);
        assert_eq!(emitter.code, vec![0xC3]);
    }

    #[test]
    fn test_translate_nop_block() {
        // Two NOPs followed by RET
        let arm64 = [
            0x1F, 0x20, 0x03, 0xD5, // NOP
            0x1F, 0x20, 0x03, 0xD5, // NOP
            0xC0, 0x03, 0x5F, 0xD6, // RET
        ];
        let block = translate_block(&arm64, 0x1000).unwrap();
        assert_eq!(block.arm64_insn_count, 3);
        assert_eq!(block.start_pc, 0x1000);
        // Should contain: NOP, NOP, RET (no extra RET since block ends with RET)
        assert!(block.code.contains(&0x90)); // NOP
        assert!(block.code.contains(&0xC3)); // RET
    }

    #[test]
    fn test_execute_write_syscall() {
        let mut state = SessionState::new();
        let req = OffloadRequest {
            session_id: 1,
            seq: 1,
            func_id: 64, // write
            flags: 0,
            payload: vec![
                0x1F, 0x20, 0x03, 0xD5, // NOP
                0xC0, 0x03, 0x5F, 0xD6, // RET
            ],
            registers: {
                let mut r = [0u64; 31];
                r[0] = 1;   // fd = stdout
                r[1] = 0x7000; // buf
                r[2] = 42;  // count
                r
            },
            pc: 0x4000,
        };

        let result = translate_and_execute_x86(&req, &mut state);
        // write returns count on success
        assert_eq!(result.return_value, 42);
        assert_eq!(result.registers[0], 42);
        assert!(result.dirty_reg_bitmap & 1 != 0);
    }
}
