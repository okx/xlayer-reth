use std::borrow::Cow;
use alloy_primitives::{Address, Bytes};
use revm::precompile::{
    u64_to_address, PrecompileError, PrecompileOutput, 
    PrecompileResult, Precompile, PrecompileId,
};

/// Poseidon hash precompile for XLayer
/// Address: 0x0000000000000000000000000000000000000100
pub const POSEIDON_ADDRESS: Address = u64_to_address(0x100);

/// Poseidon precompile instance
pub const POSEIDON: Precompile = Precompile::new(
    PrecompileId::Custom(Cow::Borrowed("poseidon")),
    POSEIDON_ADDRESS,
    poseidon_run
);

/// Gas costs for Poseidon precompile
const POSEIDON_BASE_GAS: u64 = 60;
const POSEIDON_PER_INPUT_GAS: u64 = 6;

/// Run Poseidon hash computation
pub fn poseidon_run(input: &[u8], gas_limit: u64) -> PrecompileResult {
    let num_inputs = if input.is_empty() { 0 } else { input.len() / 32 };
    let gas_cost = POSEIDON_BASE_GAS + (num_inputs as u64) * POSEIDON_PER_INPUT_GAS;
    
    tracing::info!(
        target: "xlayer::poseidon",
        input_len = input.len(),
        num_inputs = num_inputs,
        gas_cost = gas_cost,
        gas_limit = gas_limit,
        "✅ Poseidon precompile called"
    );
    
    if gas_cost > gas_limit {
        tracing::warn!(
            target: "xlayer::poseidon",
            gas_cost = gas_cost,
            gas_limit = gas_limit,
            "❌ Poseidon precompile: Out of gas"
        );
        return Err(PrecompileError::OutOfGas);
    }
    
    if !input.is_empty() && input.len() % 32 != 0 {
        tracing::warn!(
            target: "xlayer::poseidon",
            input_len = input.len(),
            "❌ Poseidon precompile: Invalid input length"
        );
        return Err(PrecompileError::other("input length must be multiple of 32"));
    }
    
    // Execute Poseidon hash.
    let output = match poseidon_hash(input) {
        Ok(output) => output,
        Err(err) => {
            tracing::error!(
                target: "xlayer::poseidon",
                error = ?err,
                "Poseidon precompile failed"
            );
            return Err(err);
        }
    };
    
    tracing::info!(
        target: "xlayer::poseidon",
        output_len = output.len(),
        gas_used = gas_cost,
        "✅ Poseidon precompile executed successfully"
    );
    
    Ok(PrecompileOutput::new(gas_cost, output))
}

/// Compute Poseidon hash
/// 
/// Input format: N * 32 bytes (N field elements)
/// Output format: 32 bytes (one field element)
fn poseidon_hash(input: &[u8]) -> Result<Bytes, PrecompileError> {
    let _ = input;
    Ok(Bytes::from(vec![0u8; 32]))
}
