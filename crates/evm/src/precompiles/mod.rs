use once_cell::sync::Lazy;
use revm::precompile::Precompiles;
use reth_optimism_forks::OpHardfork;

pub mod poseidon;


/// XLayer precompile provider
#[derive(Debug, Clone)]
pub struct XLayerPrecompiles {
    spec: OpHardfork,
}

impl XLayerPrecompiles {
    /// Create precompile provider with XLayer spec
    pub fn new(spec: OpHardfork) -> Self {
        Self { spec }
    }

    /// Get precompiles for current spec
    pub fn precompiles(&self) -> &'static Precompiles {
        // Jovian or later includes Poseidon.
        if self.spec >= OpHardfork::Jovian {
            xlayer_with_poseidon()
        } else {
            // Use standard OP Stack precompiles.
            Precompiles::latest()
        }
    }
}

/// XLayer precompiles with Poseidon (Jovian+)
fn xlayer_with_poseidon() -> &'static Precompiles {
    static INSTANCE: Lazy<Precompiles> = Lazy::new(|| {
        let mut precompiles = Precompiles::latest().clone();
        // Add Poseidon precompile.
        precompiles.extend([poseidon::POSEIDON]);
        precompiles
    });
    &INSTANCE
}

impl Default for XLayerPrecompiles {
    fn default() -> Self {
        Self::new(OpHardfork::Jovian)
    }
}
