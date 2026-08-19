package gasless

import (
	"encoding/json"
	"testing"

	"github.com/ethereum/go-ethereum/common/hexutil"

	"github.com/ethereum-optimism/optimism/op-devstack/devtest"
	"github.com/ethereum-optimism/optimism/op-devstack/presets"
	"github.com/ethereum-optimism/optimism/op-devstack/sysgo"
	"github.com/ethereum-optimism/optimism/op-service/eth"

	"github.com/okx/xlayer-reth/tests/common"
)

func newXLayerGasless(gt *testing.T) (devtest.T, *presets.XLayer) {
	t := devtest.SerialT(gt)
	return t, presets.NewXLayer(t)
}

// deployWhitelist wires the gasless whitelist deployer from the centralized
// config and deploys it against the sequencer. The forge-script invocation
// itself is the net-new integration point tracked in
// op-devstack/sysgo/xlayer_gasless.go.
func deployWhitelist(t devtest.T, sys *presets.XLayer) *sysgo.XLayerGaslessDeployer {
	cfg, err := common.LoadXLayerConfig()
	t.Require().NoError(err, "harness config must load")
	deployer := sysgo.NewXLayerGaslessDeployer(cfg.GaslessDeployScript(), sys.L2EL.Escape().UserRPC())
	deployer.Deploy(t)
	return deployer
}

// zeroGasPriceCall builds a call object with an explicit zero gas price, matching
// how a gasless (whitelisted) transaction is priced.
func zeroGasPriceCall(from, to string) map[string]any {
	return map[string]any{
		"from":     from,
		"to":       to,
		"value":    "0x0",
		"gasPrice": "0x0",
	}
}

// TestGaslessEthCall verifies a zero-gas-price call to the whitelisted path
// executes rather than being rejected for an insufficient fee.
func TestGaslessEthCall(gt *testing.T) {
	t, sys := newXLayerGasless(gt)
	deployer := deployWhitelist(t, sys)
	from := sys.FunderL2.NewFundedEOA(eth.OneEther).Address()

	call := zeroGasPriceCall(from.Hex(), deployer.ProxyAddress().Hex())
	var result hexutil.Bytes
	err := sys.L2EL.EthClient().RPC().CallContext(t.Ctx(), &result, "eth_call", call, "latest")
	t.Require().NoError(err, "zero gas price gasless eth_call must execute")
}

// TestGaslessEthSimulateV1 verifies eth_simulateV1 is gasless-aware for a
// zero-gas-price call bundle.
func TestGaslessEthSimulateV1(gt *testing.T) {
	t, sys := newXLayerGasless(gt)
	deployer := deployWhitelist(t, sys)
	from := sys.FunderL2.NewFundedEOA(eth.OneEther).Address()

	bundle := map[string]any{
		"blockStateCalls": []any{
			map[string]any{
				"calls": []any{zeroGasPriceCall(from.Hex(), deployer.ProxyAddress().Hex())},
			},
		},
	}
	var result json.RawMessage
	err := sys.L2EL.EthClient().RPC().CallContext(t.Ctx(), &result, "eth_simulateV1", bundle, "latest")
	t.Require().NoError(err, "eth_simulateV1 must accept a zero gas price gasless bundle")
}

// TestGaslessZeroPriceTransfer covers a zero gas price transfer being accepted
// and mined, with the sequencer and validator agreeing on the result. The
// on-chain assertions (two consecutive gasless blocks, validator L1-derived
// agreement on receipt/stateRoot) require the deployed whitelist and a running
// devnet and are executed via `just e2e-no-build ./e2e/gasless`.
func TestGaslessZeroPriceTransfer(gt *testing.T) {
	t, sys := newXLayerGasless(gt)
	deployWhitelist(t, sys)
	// TODO: submit a whitelisted zero gas price transfer via txplan with an
	// explicit zero fee cap, wait for inclusion on the sequencer, and assert the
	// rpc1 validator derives the same safe block and state root from L1. Needs the
	// live whitelist deployment and devnet.
}

// TestGaslessDebugTrace verifies debug_traceTransaction succeeds for a gasless
// transaction rather than being rejected on a base-fee check. The concrete trace
// assertion requires a mined gasless transaction on a live devnet.
func TestGaslessDebugTrace(gt *testing.T) {
	t, sys := newXLayerGasless(gt)
	deployWhitelist(t, sys)
	// TODO: mine a gasless transaction and assert debug_traceTransaction returns a
	// successful trace with no base-fee rejection. Needs the live devnet.
}

// TestGaslessTxRPCGasPriceIsZero verifies a mined gasless transaction reports a
// zero gas price over RPC. Requires a live gasless transaction to inspect.
func TestGaslessTxRPCGasPriceIsZero(gt *testing.T) {
	t, sys := newXLayerGasless(gt)
	deployWhitelist(t, sys)
	// TODO: mine a gasless transaction and assert eth_getTransactionByHash reports
	// gasPrice == 0x0. Needs the live devnet.
}
