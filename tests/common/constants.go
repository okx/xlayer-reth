// Package common holds the shared XLayer Go E2E harness: the single environment
// configuration entry point, the JSON-RPC client used by every scenario, the
// devnet lifecycle bootstrap, transaction/contract helpers, and synchronization
// utilities. Feature scenarios under ../e2e/<feature> consume these helpers and
// never re-read process environment or re-implement shared behavior.
package common

import "math/big"

// Wei-denominated amount building blocks shared across scenarios.
var (
	// GWei is one gwei expressed in wei.
	GWei = big.NewInt(1_000_000_000)
	// OneEther is 1e18 wei.
	OneEther = new(big.Int).Mul(GWei, GWei)
)

// DefaultGaslessGasLimit is the gas limit the gasless whitelist grants a fully
// gasless target, matching the value the legacy Rust harness used.
const DefaultGaslessGasLimit uint64 = 16_777_216

// ERC20TransferEventTopic is the topic0 of the ERC20 Transfer(address,address,uint256)
// event, used when asserting transfer logs.
const ERC20TransferEventTopic = "0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef"

// GaslessProbeInput is the 4-byte probe calldata the gasless scenarios attach to
// a zero-priced transfer so the execution client exercises the gasless path.
var GaslessProbeInput = []byte{0xde, 0xad, 0xbe, 0xef}
