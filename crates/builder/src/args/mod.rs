pub use op::{BuilderArgs, FlashblocksArgs};
use reth_optimism_cli::chainspec::OpChainSpecParser;
pub type Cli = reth_optimism_cli::Cli<OpChainSpecParser, BuilderArgs>;

mod op;
