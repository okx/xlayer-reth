package common

import "github.com/ethereum-optimism/optimism/op-service/eth"

// The harness reuses op-service/eth for all wei arithmetic rather than
// hand-rolling conversions in each scenario. These are stateless format
// conversions with no owning domain object, so they stay as package-level
// helpers per the harness code-organization rules.

// GWei converts a gwei count into the op-service wei-typed ETH value.
func GWei(gwei uint64) eth.ETH { return eth.GWei(gwei) }

// Ether converts a whole-ether count into the op-service wei-typed ETH value.
func Ether(ether uint64) eth.ETH { return eth.Ether(ether) }
