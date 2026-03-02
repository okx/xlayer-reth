// base
mod framework;
pub mod utils;

pub use framework::*;
pub use utils::*;

#[cfg(test)]
mod flashblocks;

#[cfg(test)]
mod data_availability;

#[cfg(test)]
mod miner_gas_limit;

#[cfg(test)]
mod smoke;

#[cfg(test)]
mod txpool;

#[cfg(test)]
mod forks;
// If the order of deployment from the signer changes the address will change
#[cfg(test)]
const FLASHBLOCKS_NUMBER_ADDRESS: alloy_primitives::Address =
    alloy_primitives::address!("95bd8d42f30351685e96c62eddc0d0613bf9a87a");
