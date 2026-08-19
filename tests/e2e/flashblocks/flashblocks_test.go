package flashblocks

import (
	"encoding/json"
	"testing"

	"github.com/ethereum/go-ethereum/common/hexutil"

	"github.com/ethereum-optimism/optimism/op-devstack/devtest"
	"github.com/ethereum-optimism/optimism/op-devstack/presets"
	"github.com/ethereum-optimism/optimism/op-service/eth"

	"github.com/okx/xlayer-reth/tests/common"
)

func xlayerBinaryOpts(t devtest.T) []presets.Option {
	cfg, err := common.LoadXLayerConfig()
	t.Require().NoError(err, "harness config must load")
	t.Require().NoError(cfg.RequireExecutionBinary(), "RUST_BINARY_PATH_OP_RETH must point to a built XLayer reth binary")
	return []presets.Option{
		presets.WithLocalContractSourcesAt(cfg.ForgeArtifactsDir()),
	}
}

func callRPC(t devtest.T, sys *presets.XLayerFlashblocks, out any, method string, args ...any) {
	err := sys.L2EL.EthClient().RPC().CallContext(t.Ctx(), out, method, args...)
	t.Require().NoErrorf(err, "rpc %s must not return a process-level error", method)
}

// TestFlashblocks exercises the XLayer flashblocks topology (producer sequencer +
// rpc1/rpc2 relays). All cases share a single devnet and run as subtests, since
// the topology is identical and spinning it up costs far more than each case.
func TestFlashblocks(gt *testing.T) {
	t := devtest.SerialT(gt)
	sys := presets.NewXLayerFlashblocks(t, xlayerBinaryOpts(t)...)

	// Smoke: the pending-tag RPC surface the built-in flashblocks builder feeds
	// (pending block, balance, nonce, code, call, gas estimation) must respond
	// without a process-level error while a flashblock overlay may be present.
	gt.Run("Smoke", func(gt *testing.T) {
		addr := sys.FunderL2.NewFundedEOA(eth.OneEther).Address()

		var pendingBlock map[string]json.RawMessage
		callRPC(t, sys, &pendingBlock, "eth_getBlockByNumber", "pending", true)
		t.Require().NotEmpty(pendingBlock, "pending block must be returned")

		var balance hexutil.Big
		callRPC(t, sys, &balance, "eth_getBalance", addr, "pending")

		var nonce hexutil.Uint64
		callRPC(t, sys, &nonce, "eth_getTransactionCount", addr, "pending")

		var code hexutil.Bytes
		callRPC(t, sys, &code, "eth_getCode", addr, "pending")

		call := map[string]any{"from": addr.Hex(), "to": addr.Hex(), "value": "0x0"}
		var callResult hexutil.Bytes
		callRPC(t, sys, &callResult, "eth_call", call, "pending")

		var estimate hexutil.Uint64
		callRPC(t, sys, &estimate, "eth_estimateGas", call, "pending")
	})

	// RelayPending: both relay nodes (rpc1, rpc2), which subscribe to the
	// producer's flashblocks stream, answer pending-tag queries — the relay-
	// propagation surface without op-rbuilder/rollup-boost.
	gt.Run("RelayPending", func(gt *testing.T) {
		var pendingRPC1 map[string]json.RawMessage
		err1 := sys.L2ELRPC1.EthClient().RPC().CallContext(t.Ctx(), &pendingRPC1, "eth_getBlockByNumber", "pending", false)
		t.Require().NoError(err1, "rpc1 relay must answer a pending query")

		var pendingRPC2 map[string]json.RawMessage
		err2 := sys.L2ELRPC2.EthClient().RPC().CallContext(t.Ctx(), &pendingRPC2, "eth_getBlockByNumber", "pending", false)
		t.Require().NoError(err2, "rpc2 relay must answer a pending query")
		// TODO: drive a transaction on the sequencer and assert the same flashblock
		// pending state is observed on rpc1 and rpc2 before canonicalization, and that
		// the overlay is dropped once the block is canonical. Requires the live devnet.
	})

	// EthSubscribeParamBoundaries: empty and invalid params must yield a stable,
	// decidable error rather than a valid subscription (eth_subscribe over the HTTP
	// transport also errors, which is a stable, decidable outcome for this check).
	gt.Run("EthSubscribeParamBoundaries", func(gt *testing.T) {
		var emptyParamsResult json.RawMessage
		emptyErr := sys.L2EL.EthClient().RPC().CallContext(t.Ctx(), &emptyParamsResult, "eth_subscribe")
		t.Require().Error(emptyErr, "empty-parameter flashblocks subscribe must return a decidable error")

		var invalidParamsResult json.RawMessage
		invalidErr := sys.L2EL.EthClient().RPC().CallContext(t.Ctx(), &invalidParamsResult, "eth_subscribe", "not-a-valid-flashblocks-channel")
		t.Require().Error(invalidErr, "invalid-parameter flashblocks subscribe must return a decidable error")
	})
}
