pub use op::{FlashblocksArgs, OpRbuilderArgs};
use reth_optimism_cli::chainspec::OpChainSpecParser;
pub type Cli = reth_optimism_cli::Cli<OpChainSpecParser, OpRbuilderArgs>;

mod op;
