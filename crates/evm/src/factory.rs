//! XLayer precompile factory helpers.

use revm::precompile::Precompiles;
use reth_optimism_forks::OpHardfork;

use crate::precompiles::XLayerPrecompiles;

/// Get XLayer precompiles for the given hardfork.
pub fn xlayer_precompiles(hardfork: OpHardfork) -> &'static Precompiles {
    let precompiles = XLayerPrecompiles::new(hardfork).precompiles();
    
    tracing::trace!(
        target: "xlayer::factory",
        hardfork = ?hardfork,
        precompiles_count = precompiles.len(),
        "📦 Loading XLayer precompiles"
    );
    
    precompiles
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_xlayer_precompiles() {
        let precompiles = xlayer_precompiles(OpHardfork::Jovian);
        assert!(precompiles.len() > 0);
    }
}
