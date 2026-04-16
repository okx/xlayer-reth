//! Bytecode purity analysis for EIP-8130 custom verifiers.
//!
//! A verifier contract is **pure** when its `verify(bytes32, bytes)` call is
//! a deterministic function of its inputs alone — no state reads, no
//! environment-dependent opcodes, and external calls only to known precompiles.

use std::collections::BTreeSet;

/// Maximum standard precompile address.
const MAX_STANDARD_PRECOMPILE: u16 = 0x12;

/// P256VERIFY precompile address (RIP-7212).
const P256VERIFY_ADDR: u16 = 0x100;

/// EIP-8130 TxContext precompile address.
const TX_CONTEXT_ADDR: u64 = 0xaa03;

/// Result of bytecode purity analysis.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PurityVerdict {
    /// The bytecode is pure.
    Pure {
        /// Precompile addresses called via `STATICCALL`.
        precompile_calls: Vec<u16>,
    },
    /// The bytecode is not pure.
    Impure {
        /// Reasons for rejection.
        reasons: Vec<PurityViolation>,
    },
}

impl PurityVerdict {
    /// Returns `true` if the bytecode was determined to be pure.
    pub fn is_pure(&self) -> bool {
        matches!(self, Self::Pure { .. })
    }
}

/// A single reason why bytecode is not pure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PurityViolation {
    /// A forbidden opcode was found.
    BannedOpcode { offset: usize, opcode: u8, category: ViolationCategory },
    /// A `STATICCALL` target could not be statically determined.
    DynamicStaticCallTarget { offset: usize },
    /// A `STATICCALL` to a non-precompile address.
    NonPrecompileStaticCall { offset: usize, target: u64 },
    /// `GAS` opcode used outside the `STATICCALL` convention.
    StandaloneGas { offset: usize },
    /// Bytecode is empty.
    EmptyBytecode,
    /// A forbidden opcode in a hidden instruction stream.
    HiddenJumpdestViolation { hidden_jumpdest_offset: usize, opcode_offset: usize, opcode: u8 },
}

/// Category of a forbidden opcode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViolationCategory {
    /// Reads or writes storage.
    StateAccess,
    /// Non-deterministic environment read.
    NonDeterministicEnv,
    /// Side effects (logs, creates, etc.).
    SideEffect,
    /// Not on the explicit allowlist.
    ForbiddenOpcode,
}

/// A bytecode purity scanner.
#[derive(Debug)]
pub struct PurityScanner;

impl PurityScanner {
    /// Analyze deployed bytecode and return a purity verdict.
    pub fn analyze(bytecode: &[u8]) -> PurityVerdict {
        if bytecode.is_empty() {
            return PurityVerdict::Impure { reasons: vec![PurityViolation::EmptyBytecode] };
        }

        let insns = disassemble(bytecode);
        let skip_offsets = build_unreachable_set(&insns);

        let mut violations = Vec::new();
        let mut precompile_calls = Vec::new();

        for (idx, insn) in insns.iter().enumerate() {
            if skip_offsets.contains(&insn.offset) {
                continue;
            }

            if insn.opcode == op::STATICCALL {
                match resolve_staticcall_target(&insns, idx) {
                    StaticCallTarget::Precompile(addr) => precompile_calls.push(addr),
                    StaticCallTarget::NonPrecompile(addr) => {
                        violations.push(PurityViolation::NonPrecompileStaticCall {
                            offset: insn.offset,
                            target: addr,
                        });
                    }
                    StaticCallTarget::Dynamic => {
                        violations
                            .push(PurityViolation::DynamicStaticCallTarget { offset: insn.offset });
                    }
                }
            } else if insn.opcode == op::GAS {
                let is_before_staticcall = idx + 1 < insns.len()
                    && insns[idx + 1].opcode == op::STATICCALL
                    && !skip_offsets.contains(&insns[idx + 1].offset);
                if !is_before_staticcall {
                    violations.push(PurityViolation::StandaloneGas { offset: insn.offset });
                }
            } else if !is_allowed_opcode(insn.opcode) {
                violations.push(PurityViolation::BannedOpcode {
                    offset: insn.offset,
                    opcode: insn.opcode,
                    category: violation_category(insn.opcode),
                });
            }
        }

        violations.extend(scan_hidden_jumpdests(bytecode, &insns));

        if violations.is_empty() {
            PurityVerdict::Pure { precompile_calls }
        } else {
            PurityVerdict::Impure { reasons: violations }
        }
    }
}

// ── Opcode constants ────────────────────────────────────────────────────

mod op {
    pub(super) const STOP: u8 = 0x00;
    pub(super) const PUSH0: u8 = 0x5F;
    pub(super) const PUSH1: u8 = 0x60;
    pub(super) const PUSH32: u8 = 0x7F;
    pub(super) const GAS: u8 = 0x5A;
    pub(super) const JUMPDEST: u8 = 0x5B;
    pub(super) const STATICCALL: u8 = 0xFA;
    pub(super) const RETURN: u8 = 0xF3;
    pub(super) const REVERT: u8 = 0xFD;
    pub(super) const INVALID: u8 = 0xFE;
}

// ── Opcode allowlist ─────────────────────────────────────────────────────

fn is_allowed_opcode(opcode: u8) -> bool {
    matches!(
        opcode,
        0x00        |       // STOP
        0x01..=0x0B |       // arithmetic
        0x10..=0x1D |       // comparison + bitwise
        0x20        |       // SHA3
        0x30        |       // ADDRESS
        0x33..=0x39 |       // CALLER..CODECOPY
        0x3D..=0x3E |       // RETURNDATASIZE, RETURNDATACOPY
        0x46        |       // CHAINID
        0x50..=0x53 |       // POP, MLOAD, MSTORE, MSTORE8
        0x56..=0x59 |       // JUMP, JUMPI, PC, MSIZE
        0x5B        |       // JUMPDEST
        0x5E..=0x7F |       // MCOPY, PUSH0..PUSH32
        0x80..=0x9F |       // DUP1..SWAP16
        0xF3        |       // RETURN
        0xFD..=0xFE         // REVERT, INVALID
    )
}

fn violation_category(opcode: u8) -> ViolationCategory {
    match opcode {
        0x31 | 0x3B | 0x3C | 0x3F | 0x47 | 0x54 | 0x55 | 0x5C | 0x5D => {
            ViolationCategory::StateAccess
        }
        0x32 | 0x3A | 0x40..=0x45 | 0x48..=0x4A => ViolationCategory::NonDeterministicEnv,
        0xA0..=0xA4 | 0xF0..=0xF2 | 0xF4..=0xF5 | 0xFF => ViolationCategory::SideEffect,
        _ => ViolationCategory::ForbiddenOpcode,
    }
}

fn is_known_precompile(addr: u64) -> bool {
    (1..=MAX_STANDARD_PRECOMPILE as u64).contains(&addr)
        || addr == P256VERIFY_ADDR as u64
        || addr == TX_CONTEXT_ADDR
}

// ── Disassembler ────────────────────────────────────────────────────────

struct Instruction {
    offset: usize,
    opcode: u8,
    push_value: Option<u64>,
    push_size: usize,
}

fn disassemble(code: &[u8]) -> Vec<Instruction> {
    let mut insns = Vec::new();
    let mut i = 0;
    while i < code.len() {
        let opcode = code[i];
        if (op::PUSH1..=op::PUSH32).contains(&opcode) {
            let push_size = (opcode - op::PUSH0) as usize;
            let end = core::cmp::min(i + 1 + push_size, code.len());
            let bytes = &code[i + 1..end];
            let value = if bytes.len() <= 8 {
                let mut buf = [0u8; 8];
                buf[8 - bytes.len()..].copy_from_slice(bytes);
                u64::from_be_bytes(buf)
            } else {
                let high_bytes = &bytes[..bytes.len() - 8];
                if high_bytes.iter().all(|&b| b == 0) {
                    let mut buf = [0u8; 8];
                    buf.copy_from_slice(&bytes[bytes.len() - 8..]);
                    u64::from_be_bytes(buf)
                } else {
                    u64::MAX
                }
            };
            insns.push(Instruction { offset: i, opcode, push_value: Some(value), push_size });
            i = end;
        } else if opcode == op::PUSH0 {
            insns.push(Instruction { offset: i, opcode, push_value: Some(0), push_size: 0 });
            i += 1;
        } else {
            insns.push(Instruction { offset: i, opcode, push_value: None, push_size: 0 });
            i += 1;
        }
    }
    insns
}

fn build_unreachable_set(insns: &[Instruction]) -> BTreeSet<usize> {
    let mut skip = BTreeSet::new();
    let mut unreachable = false;
    for insn in insns {
        if unreachable {
            if insn.opcode == op::JUMPDEST {
                unreachable = false;
            } else {
                skip.insert(insn.offset);
                continue;
            }
        }
        if matches!(insn.opcode, op::STOP | op::RETURN | op::REVERT | op::INVALID) {
            unreachable = true;
        }
    }
    skip
}

// ── Hidden JUMPDEST scanner ─────────────────────────────────────────────

fn scan_hidden_jumpdests(code: &[u8], insns: &[Instruction]) -> Vec<PurityViolation> {
    let mut violations = Vec::new();
    for insn in insns {
        if insn.push_size == 0 {
            continue;
        }
        for j in 1..=insn.push_size {
            let hidden_off = insn.offset + j;
            if hidden_off >= code.len() || code[hidden_off] != op::JUMPDEST {
                continue;
            }
            let alt_insns = disassemble(&code[hidden_off..]);
            for alt in alt_insns.iter().skip(1) {
                let abs = hidden_off + alt.offset;
                if alt.opcode == op::STATICCALL || alt.opcode == op::GAS {
                    violations.push(PurityViolation::HiddenJumpdestViolation {
                        hidden_jumpdest_offset: hidden_off,
                        opcode_offset: abs,
                        opcode: alt.opcode,
                    });
                    break;
                }
                if !is_allowed_opcode(alt.opcode) {
                    violations.push(PurityViolation::HiddenJumpdestViolation {
                        hidden_jumpdest_offset: hidden_off,
                        opcode_offset: abs,
                        opcode: alt.opcode,
                    });
                    break;
                }
                if matches!(alt.opcode, op::STOP | op::RETURN | op::REVERT | op::INVALID) {
                    break;
                }
            }
        }
    }
    violations
}

// ── STATICCALL target resolution ────────────────────────────────────────

enum StaticCallTarget {
    Precompile(u16),
    NonPrecompile(u64),
    Dynamic,
}

fn resolve_staticcall_target(insns: &[Instruction], sc_idx: usize) -> StaticCallTarget {
    if sc_idx < 2 {
        return StaticCallTarget::Dynamic;
    }
    let gas_insn = &insns[sc_idx - 1];
    let addr_insn = &insns[sc_idx - 2];
    if gas_insn.opcode != op::GAS {
        return StaticCallTarget::Dynamic;
    }
    match addr_insn.push_value {
        Some(addr) if is_known_precompile(addr) => StaticCallTarget::Precompile(addr as u16),
        Some(addr) => StaticCallTarget::NonPrecompile(addr),
        None => StaticCallTarget::Dynamic,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_bytecode_is_impure() {
        let verdict = PurityScanner::analyze(&[]);
        assert!(!verdict.is_pure());
    }

    #[test]
    fn minimal_pure_bytecode() {
        let code = [0x60, 0x00, 0x60, 0x00, 0xF3];
        assert!(PurityScanner::analyze(&code).is_pure());
    }

    #[test]
    fn sload_is_impure() {
        let code = [0x60, 0x00, 0x54, 0x50, 0x60, 0x00, 0x60, 0x00, 0xF3];
        let verdict = PurityScanner::analyze(&code);
        assert!(!verdict.is_pure());
        match verdict {
            PurityVerdict::Impure { reasons } => {
                assert!(reasons.iter().any(|r| matches!(
                    r,
                    PurityViolation::BannedOpcode {
                        opcode: 0x54,
                        category: ViolationCategory::StateAccess,
                        ..
                    }
                )));
            }
            _ => panic!("expected impure"),
        }
    }

    #[test]
    fn staticcall_to_ecrecover_is_pure() {
        #[rustfmt::skip]
        let code = [
            0x60, 0x20, 0x60, 0x00, 0x60, 0x80, 0x60, 0x00,
            0x60, 0x01, // PUSH1 addr = ecrecover
            0x5A, 0xFA, // GAS, STATICCALL
            0x50, 0x60, 0x20, 0x60, 0x00, 0xF3,
        ];
        let verdict = PurityScanner::analyze(&code);
        assert!(verdict.is_pure());
        match verdict {
            PurityVerdict::Pure { precompile_calls } => assert_eq!(precompile_calls, vec![1]),
            _ => panic!("expected pure"),
        }
    }

    #[test]
    fn staticcall_to_non_precompile_is_impure() {
        #[rustfmt::skip]
        let code = [
            0x60, 0x20, 0x60, 0x00, 0x60, 0x80, 0x60, 0x00,
            0x61, 0x02, 0x00,
            0x5A, 0xFA,
            0x50, 0x60, 0x20, 0x60, 0x00, 0xF3,
        ];
        assert!(!PurityScanner::analyze(&code).is_pure());
    }

    #[test]
    fn standalone_gas_is_impure() {
        let code = [0x5A, 0x50, 0x60, 0x00, 0x60, 0x00, 0xF3];
        assert!(!PurityScanner::analyze(&code).is_pure());
    }

    #[test]
    fn hidden_jumpdest_sload_in_push() {
        #[rustfmt::skip]
        let code = [
            0x61, 0x5B, 0x54, // PUSH2 0x5B54 (hides JUMPDEST+SLOAD)
            0x50, 0x60, 0x01, 0x56, 0x00,
        ];
        let verdict = PurityScanner::analyze(&code);
        assert!(!verdict.is_pure());
        match verdict {
            PurityVerdict::Impure { reasons } => {
                assert!(reasons
                    .iter()
                    .any(|r| matches!(r, PurityViolation::HiddenJumpdestViolation { .. })));
            }
            _ => panic!("expected impure"),
        }
    }

    #[test]
    fn unreachable_code_after_return_is_skipped() {
        #[rustfmt::skip]
        let code = [
            0x60, 0x00, 0x60, 0x00, 0xF3,
            0xFF, 0xFF, 0x54, 0x42, 0xFA, // unreachable
        ];
        assert!(PurityScanner::analyze(&code).is_pure());
    }

    #[test]
    fn staticcall_to_tx_context_is_pure() {
        #[rustfmt::skip]
        let code = [
            0x60, 0x20, 0x60, 0x00, 0x60, 0x80, 0x60, 0x00,
            0x61, 0xAA, 0x03, // PUSH2 0xAA03
            0x5A, 0xFA,
            0x50, 0x60, 0x20, 0x60, 0x00, 0xF3,
        ];
        let verdict = PurityScanner::analyze(&code);
        assert!(verdict.is_pure(), "TxContext precompile should be safe, got: {verdict:?}");
    }
}
