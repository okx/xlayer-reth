//! Integration-test binary for `rcs_filter::capture`.

pub(crate) use alloy_primitives::{keccak256, Address, Bytes, Log, LogData, B256, U256};
pub(crate) use rcs_filter::callstack::{CallObservation, SnapshotStamp};
pub(crate) use rcs_filter::capture::*;
pub(crate) use revm::interpreter::InstructionResult;
pub(crate) use revm::{bytecode::opcode, inspector::NoOpInspector, interpreter::CallScheme};

mod callstack_observation_tests;
mod gating_integration_tests;
mod test_fixtures;
mod tests;
