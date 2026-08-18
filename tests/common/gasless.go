package common

import (
	"context"
	"fmt"
	"math/big"

	"github.com/ethereum/go-ethereum/common"
)

// GaslessConfig identifies the on-chain gasless predeploy the whitelist calls
// target. The address is supplied by the caller (from the XLayer chain
// configuration) rather than hard-coded into a scenario.
type GaslessConfig struct {
	// Contract is the gasless predeploy address that owns the enable/whitelist API.
	Contract common.Address
	// GasLimit is the per-target gas allowance granted when whitelisting.
	GasLimit *big.Int
}

// EnableGasless sends setGaslessEnabled(true) to the gasless predeploy and waits
// for the owner transaction to be mined.
func (a *Account) EnableGasless(ctx context.Context, c *DevnetClient, g GaslessConfig, gasLimit uint64, gasFeeCap *big.Int) error {
	h, err := a.CallContract(ctx, c, g.Contract, EncodeSetGaslessEnabled(true), gasLimit, big.NewInt(0), gasFeeCap)
	if err != nil {
		return fmt.Errorf("setGaslessEnabled: %w", err)
	}
	if _, err := c.WaitForTxMined(ctx, h); err != nil {
		return fmt.Errorf("await setGaslessEnabled: %w", err)
	}
	return nil
}

// WhitelistFullyGaslessTarget sends setFullyGaslessTarget(target, true, gasLimit)
// to the gasless predeploy and waits for the owner transaction to be mined. It
// ports the Rust ensure_gasless_whitelist setup flow.
func (a *Account) WhitelistFullyGaslessTarget(ctx context.Context, c *DevnetClient, g GaslessConfig, target common.Address, gasLimit uint64, gasFeeCap *big.Int) error {
	limit := g.GasLimit
	if limit == nil {
		limit = new(big.Int).SetUint64(DefaultGaslessGasLimit)
	}
	h, err := a.CallContract(ctx, c, g.Contract, EncodeSetFullyGaslessTarget(target, true, limit), gasLimit, big.NewInt(0), gasFeeCap)
	if err != nil {
		return fmt.Errorf("setFullyGaslessTarget: %w", err)
	}
	if _, err := c.WaitForTxMined(ctx, h); err != nil {
		return fmt.Errorf("await setFullyGaslessTarget: %w", err)
	}
	return nil
}

// EnsureGaslessWhitelist enables gasless execution and whitelists target in one
// step, matching the combined precondition the gasless scenarios rely on.
func (a *Account) EnsureGaslessWhitelist(ctx context.Context, c *DevnetClient, g GaslessConfig, target common.Address, gasLimit uint64, gasFeeCap *big.Int) error {
	if err := a.EnableGasless(ctx, c, g, gasLimit, gasFeeCap); err != nil {
		return err
	}
	return a.WhitelistFullyGaslessTarget(ctx, c, g, target, gasLimit, gasFeeCap)
}
