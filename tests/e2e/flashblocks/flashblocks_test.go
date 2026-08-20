package flashblocks

import (
	"context"
	"encoding/json"
	"math/big"
	"slices"
	"testing"
	"time"

	"github.com/ethereum/go-ethereum/common"
	"github.com/ethereum/go-ethereum/common/hexutil"

	"github.com/ethereum-optimism/optimism/op-devstack/devtest"
	"github.com/ethereum-optimism/optimism/op-devstack/presets"
	"github.com/ethereum-optimism/optimism/op-service/client"
	"github.com/ethereum-optimism/optimism/op-service/eth"
	"github.com/ethereum-optimism/optimism/op-service/txplan"

	xcommon "github.com/okx/xlayer-reth/tests/common"
)

func xlayerBinaryOpts(t devtest.T) []presets.Option {
	cfg, err := xcommon.LoadXLayerConfig()
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
// rpc1/rpc2 relays). All cases share a single devnet and run concurrently as
// subtests, since the topology is identical and spinning it up costs far more
// than each case.
func TestFlashblocks(gt *testing.T) {
	t := devtest.ParallelT(gt)
	sys := presets.NewXLayerFlashblocks(t, xlayerBinaryOpts(t)...)

	// Smoke: the pending-tag RPC surface the built-in flashblocks builder feeds
	// (pending block, balance, nonce, code, call, gas estimation) must respond
	// without a process-level error while a flashblock overlay may be present.
	gt.Run("Smoke", func(gt *testing.T) {
		t := devtest.ParallelT(gt)
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

	// RelayPending verifies that rpc1 and rpc2 observe the same transaction and
	// state through the producer's flashblock stream before canonicalization,
	// then discard that transaction from their pending overlays after inclusion.
	gt.Run("RelayPending", func(gt *testing.T) {
		t := devtest.ParallelT(gt)
		sender := sys.FunderL2.NewFundedEOA(eth.OneEther)
		recipient := sys.Wallet.NewEOA(sys.L2EL).Address()
		value := eth.OneHundredthEther

		planned := txplan.NewPlannedTx(sender.Plan(), txplan.WithTo(&recipient), txplan.WithValue(value))
		signed, err := planned.Signed.Eval(t.Ctx())
		t.Require().NoError(err, "sign relay propagation transaction")
		_, err = planned.Submitted.Eval(t.Ctx())
		t.Require().NoError(err, "submit relay propagation transaction")
		txHash := signed.Hash()

		type pendingState struct {
			blockHash    common.Hash
			blockNumber  hexutil.Uint64
			transactions []common.Hash
			balance      *big.Int
		}
		readPending := func(rpcClient interface {
			CallContext(ctx context.Context, result any, method string, args ...any) error
		}) (pendingState, error) {
			var block struct {
				Hash         common.Hash    `json:"hash"`
				Number       hexutil.Uint64 `json:"number"`
				Transactions []common.Hash  `json:"transactions"`
			}
			if err := rpcClient.CallContext(t.Ctx(), &block, "eth_getBlockByNumber", "pending", false); err != nil {
				return pendingState{}, err
			}
			var balance hexutil.Big
			if err := rpcClient.CallContext(t.Ctx(), &balance, "eth_getBalance", recipient, "pending"); err != nil {
				return pendingState{}, err
			}
			return pendingState{
				blockHash:    block.Hash,
				blockNumber:  block.Number,
				transactions: block.Transactions,
				balance:      new(big.Int).Set((*big.Int)(&balance)),
			}, nil
		}
		contains := func(hashes []common.Hash, want common.Hash) bool {
			return slices.Contains(hashes, want)
		}

		var rpc1Pending, rpc2Pending pendingState
		t.Require().Eventually(func() bool {
			var err1, err2 error
			rpc1Pending, err1 = readPending(sys.L2ELRPC1.EthClient().RPC())
			rpc2Pending, err2 = readPending(sys.L2ELRPC2.EthClient().RPC())
			return err1 == nil && err2 == nil &&
				contains(rpc1Pending.transactions, txHash) && contains(rpc2Pending.transactions, txHash) &&
				rpc1Pending.balance.Cmp(value.ToBig()) == 0 && rpc2Pending.balance.Cmp(value.ToBig()) == 0
		}, 3*time.Second, 20*time.Millisecond, "both relays must expose the transaction's flashblock state before canonicalization")
		t.Require().Equal(rpc1Pending.blockNumber, rpc2Pending.blockNumber, "relays must expose the same pending block number")
		t.Require().Equal(rpc1Pending.blockHash, rpc2Pending.blockHash, "relays must expose the same pending block hash")

		_, err = planned.Success.Eval(t.Ctx())
		t.Require().NoError(err, "relay propagation transaction must become canonical")
		t.Require().Eventually(func() bool {
			state1, err1 := readPending(sys.L2ELRPC1.EthClient().RPC())
			state2, err2 := readPending(sys.L2ELRPC2.EthClient().RPC())
			return err1 == nil && err2 == nil &&
				!contains(state1.transactions, txHash) && !contains(state2.transactions, txHash)
		}, 10*time.Second, 100*time.Millisecond, "canonical transaction must be removed from both relay pending overlays")
	})

	// EthSubscribeParamBoundaries uses its own WebSocket stream to the shared
	// sequencer. Empty and invalid params must yield a stable, decidable error
	// rather than a valid subscription, without requiring another node topology.
	gt.Run("EthSubscribeParamBoundaries", func(gt *testing.T) {
		t := devtest.ParallelT(gt)
		stream, err := client.NewRPC(t.Ctx(), t.Logger(), sys.L2EL.Escape().UserRPC(), client.WithLazyDial())
		t.Require().NoError(err, "open dedicated eth_subscribe stream")
		t.Cleanup(stream.Close)

		var emptyParamsResult json.RawMessage
		emptyErr := stream.CallContext(t.Ctx(), &emptyParamsResult, "eth_subscribe")
		t.Require().Error(emptyErr, "empty-parameter flashblocks subscribe must return a decidable error")

		var invalidParamsResult json.RawMessage
		invalidErr := stream.CallContext(t.Ctx(), &invalidParamsResult, "eth_subscribe", "not-a-valid-flashblocks-channel")
		t.Require().Error(invalidErr, "invalid-parameter flashblocks subscribe must return a decidable error")
	})
}
