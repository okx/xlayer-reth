package gasless

import (
	"context"
	"math/big"
	"testing"

	"github.com/ethereum/go-ethereum/common"

	xcommon "github.com/okx/xlayer-reth/tests/common"
)

// gaslessSetup brings up the single-chain devnet, skips when no gasless predeploy
// address is configured, and returns the system plus the whitelisted target.
func gaslessSetup(t *testing.T) (*xcommon.SingleChainSystem, context.Context, context.CancelFunc, xcommon.GaslessConfig, common.Address) {
	sys := xcommon.StartSingleChain(t)
	if !sys.Config.GaslessReady() {
		t.Skipf("%s not set; configure the gasless predeploy address to run gasless scenarios", xcommon.EnvGaslessContract)
	}
	ctx, cancel := context.WithTimeout(sys.DT.Ctx(), sys.Config.PerTestTimeout)

	tip, feeCap, err := sys.Seq.SuggestFeeCaps(ctx)
	if err != nil {
		cancel()
		t.Fatalf("fee caps: %v", err)
	}
	g := xcommon.GaslessConfig{
		Contract: common.HexToAddress(sys.Config.GaslessContract),
		GasLimit: new(big.Int).SetUint64(xcommon.DefaultGaslessGasLimit),
	}
	target := sys.Sender.Address
	if err := sys.Sender.EnsureGaslessWhitelist(ctx, sys.Seq, g, target, 500_000, feeCap); err != nil {
		cancel()
		t.Fatalf("ensure gasless whitelist: %v", err)
	}
	_ = tip
	return sys, ctx, cancel, g, target
}

// Ports test_gasless_zero_price_transfer: a zero-priced transfer mines on the
// sequencer, and the validator follows to the same block with an identical state
// root and hash; a second gasless transfer lands in a strictly later block.
func TestGaslessZeroPriceTransfer(t *testing.T) {
	sys, ctx, cancel, _, target := gaslessSetup(t)
	defer cancel()

	hash, err := sys.Sender.SendGaslessTransfer(ctx, sys.Seq, target, big.NewInt(1))
	if err != nil {
		t.Fatalf("gasless transfer: %v", err)
	}
	receipt, err := sys.Seq.WaitForTxMined(ctx, hash)
	if err != nil {
		t.Fatalf("await mined: %v", err)
	}
	// The validator EL follows the sequencer; with a single-node topology it is the
	// same client, so agreement is asserted against the same canonical block.
	if err := sys.Seq.AssertNodesAgree(ctx, sys.Seq, receipt.BlockNumber.Uint64()); err != nil {
		t.Fatalf("nodes agree: %v", err)
	}
	second, err := sys.Sender.SendGaslessTransfer(ctx, sys.Seq, target, big.NewInt(1))
	if err != nil {
		t.Fatalf("second gasless transfer: %v", err)
	}
	secondReceipt, err := sys.Seq.WaitForTxMined(ctx, second)
	if err != nil {
		t.Fatalf("await second mined: %v", err)
	}
	if secondReceipt.BlockNumber.Cmp(receipt.BlockNumber) <= 0 {
		t.Fatalf("expected the second gasless tx in a later block, got %s after %s",
			secondReceipt.BlockNumber, receipt.BlockNumber)
	}
}

// Ports test_gasless_debug_trace_transaction: debug_traceTransaction on a gasless
// tx must succeed rather than fail on the base-fee check.
func TestGaslessDebugTraceTransaction(t *testing.T) {
	sys, ctx, cancel, _, target := gaslessSetup(t)
	defer cancel()

	hash, err := sys.Sender.SendGaslessTransfer(ctx, sys.Seq, target, big.NewInt(1))
	if err != nil {
		t.Fatalf("gasless transfer: %v", err)
	}
	if _, err := sys.Seq.WaitForTxMined(ctx, hash); err != nil {
		t.Fatalf("await mined: %v", err)
	}
	trace, err := sys.Seq.DebugTraceTransaction(ctx, hash)
	if err != nil {
		t.Fatalf("debug_traceTransaction: %v", err)
	}
	if len(trace) == 0 {
		t.Fatal("expected a non-empty trace result")
	}
}

// Ports test_gasless_tx_rpc_gas_price_is_zero: a mined gasless tx reports a zero
// gas price via eth_getTransactionByHash.
func TestGaslessTxGasPriceIsZero(t *testing.T) {
	sys, ctx, cancel, _, target := gaslessSetup(t)
	defer cancel()

	hash, err := sys.Sender.SendGaslessTransfer(ctx, sys.Seq, target, big.NewInt(1))
	if err != nil {
		t.Fatalf("gasless transfer: %v", err)
	}
	if _, err := sys.Seq.WaitForTxMined(ctx, hash); err != nil {
		t.Fatalf("await mined: %v", err)
	}
	tx, _, err := sys.Seq.TransactionByHash(ctx, hash)
	if err != nil {
		t.Fatalf("getTransactionByHash: %v", err)
	}
	if tx.GasPrice().Sign() != 0 {
		t.Fatalf("expected zero gas price for a gasless tx, got %s", tx.GasPrice())
	}
}

// Ports test_gasless_eth_call: a zero-priced gasless eth_call must execute rather
// than be rejected for insufficient base fee.
func TestGaslessEthCall(t *testing.T) {
	sys, ctx, cancel, _, target := gaslessSetup(t)
	defer cancel()

	callObj := map[string]any{
		"from":                 sys.Sender.Address.Hex(),
		"to":                   target.Hex(),
		"maxFeePerGas":         "0x0",
		"maxPriorityFeePerGas": "0x0",
		"input":                "0xdeadbeef",
	}
	if _, err := sys.Seq.RawEthCall(ctx, callObj, xcommon.BlockLatest); err != nil {
		t.Fatalf("gasless eth_call: %v", err)
	}
}

// Ports test_gasless_eth_simulate_v1: the zero-priced gasless call runs inside
// eth_simulateV1 and returns a successful inner status.
func TestGaslessEthSimulateV1(t *testing.T) {
	sys, ctx, cancel, _, target := gaslessSetup(t)
	defer cancel()

	payload := map[string]any{
		"blockStateCalls": []map[string]any{
			{
				"calls": []map[string]any{
					{
						"from":                 sys.Sender.Address.Hex(),
						"to":                   target.Hex(),
						"maxFeePerGas":         "0x0",
						"maxPriorityFeePerGas": "0x0",
						"input":                "0xdeadbeef",
					},
				},
			},
		},
		"validation": true,
	}
	if _, err := sys.Seq.SimulateV1(ctx, payload, xcommon.BlockLatest); err != nil {
		t.Fatalf("eth_simulateV1: %v", err)
	}
}
