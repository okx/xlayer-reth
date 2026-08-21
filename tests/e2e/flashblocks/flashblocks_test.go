package flashblocks

import (
	"context"
	"encoding/json"
	"testing"
	"time"

	"github.com/ethereum/go-ethereum"
	"github.com/ethereum/go-ethereum/common"
	"github.com/ethereum/go-ethereum/common/hexutil"
	"github.com/ethereum/go-ethereum/core/types"

	"github.com/ethereum-optimism/optimism/op-devstack/devtest"
	"github.com/ethereum-optimism/optimism/op-devstack/presets"
	"github.com/ethereum-optimism/optimism/op-service/apis"
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

type flashblockSubscriptionEvent struct {
	Type        string                        `json:"type"`
	Header      *types.Header                 `json:"header,omitempty"`
	Transaction flashblockTransactionEnvelope `json:"transaction,omitempty"`
}

// flashblockTransactionEnvelope mirrors the flashblocks-specific event wrapper.
// Its payload uses geth's canonical receipt type instead of duplicating RPC
// receipt fields locally.
type flashblockTransactionEnvelope struct {
	TxHash  common.Hash    `json:"txHash"`
	Receipt *types.Receipt `json:"receipt,omitempty"`
}

func subscribeRelayTransactions(t devtest.T, endpoint string, address common.Address) (<-chan flashblockSubscriptionEvent, <-chan error) {
	rpcClient, err := client.NewRPC(t.Ctx(), t.Logger(), endpoint, client.WithLazyDial())
	t.Require().NoError(err, "open relay flashblocks subscription stream")
	t.Cleanup(rpcClient.Close)

	events := make(chan flashblockSubscriptionEvent, 64)
	filter := map[string]any{
		"headerInfo": true,
		"subTxFilter": map[string]any{
			"subscribeAddresses": []common.Address{address},
			"txReceipt":          true,
		},
	}
	subscription, err := rpcClient.Subscribe(t.Ctx(), "eth", events, "flashblocks", filter)
	t.Require().NoError(err, "subscribe to executed relay flashblocks")
	t.Cleanup(subscription.Unsubscribe)
	return events, subscription.Err()
}

func waitForRelayExecution(t devtest.T, events <-chan flashblockSubscriptionEvent, subscriptionErr <-chan error, relay string) {
	ctx, cancel := context.WithTimeout(t.Ctx(), 5*time.Second)
	defer cancel()
	for {
		select {
		case event, ok := <-events:
			t.Require().True(ok, "%s flashblocks event channel closed", relay)
			if event.Type == "header" && event.Header != nil && event.Header.Number != nil && event.Header.Hash() != (common.Hash{}) {
				return
			}
		case err, ok := <-subscriptionErr:
			t.Require().True(ok, "%s flashblocks subscription error channel closed", relay)
			t.Require().NoError(err, "%s flashblocks subscription failed", relay)
		case <-ctx.Done():
			t.Require().NoError(ctx.Err(), "timed out waiting for %s to execute a flashblock", relay)
		}
	}
}

func waitForCommonCanonicalBase(t devtest.T, sequencer, rpc1, rpc2 apis.EthClient) {
	ctx, cancel := context.WithTimeout(t.Ctx(), 10*time.Second)
	defer cancel()
	ticker := time.NewTicker(20 * time.Millisecond)
	defer ticker.Stop()

	var sequencerHead, rpc1Head, rpc2Head *types.Header
	var sequencerErr, rpc1Err, rpc2Err error
	readLatest := func(rpcClient apis.EthClient) (*types.Header, error) {
		return rpcClient.HeaderByLabel(ctx, eth.Unsafe)
	}
	headRef := func(header *types.Header) (uint64, common.Hash) {
		if header == nil || header.Number == nil {
			return 0, common.Hash{}
		}
		return header.Number.Uint64(), header.Hash()
	}
	for {
		sequencerHead, sequencerErr = readLatest(sequencer)
		rpc1Head, rpc1Err = readLatest(rpc1)
		rpc2Head, rpc2Err = readLatest(rpc2)
		sequencerNumber, sequencerHash := headRef(sequencerHead)
		rpc1Number, rpc1Hash := headRef(rpc1Head)
		rpc2Number, rpc2Hash := headRef(rpc2Head)
		if sequencerErr == nil && rpc1Err == nil && rpc2Err == nil &&
			sequencerHash != (common.Hash{}) &&
			sequencerHash == rpc1Hash && sequencerHash == rpc2Hash {
			return
		}

		select {
		case <-ctx.Done():
			t.Require().NoError(ctx.Err(),
				"timed out waiting for a common canonical flashblock base (sequencer=%d/%s err=%v, rpc1=%d/%s err=%v, rpc2=%d/%s err=%v)",
				sequencerNumber, sequencerHash, sequencerErr,
				rpc1Number, rpc1Hash, rpc1Err,
				rpc2Number, rpc2Hash, rpc2Err)
		case <-ticker.C:
		}
	}
}

func waitForRelayTransaction(t devtest.T, events <-chan flashblockSubscriptionEvent, subscriptionErr <-chan error, relay string, txHash common.Hash) flashblockSubscriptionEvent {
	ctx, cancel := context.WithTimeout(t.Ctx(), 5*time.Second)
	defer cancel()
	var lastHeaderNumber uint64
	for {
		select {
		case event, ok := <-events:
			t.Require().True(ok, "%s flashblocks event channel closed", relay)
			if event.Type == "header" && event.Header != nil && event.Header.Number != nil {
				lastHeaderNumber = event.Header.Number.Uint64()
			}
			if event.Type == "transaction" && event.Transaction.TxHash == txHash {
				return event
			}
		case err, ok := <-subscriptionErr:
			t.Require().True(ok, "%s flashblocks subscription error channel closed", relay)
			t.Require().NoError(err, "%s flashblocks subscription failed", relay)
		case <-ctx.Done():
			t.Require().NoError(ctx.Err(), "timed out waiting for %s flashblock transaction %s (last executed header=%d)", relay, txHash, lastHeaderNumber)
		}
	}
}

func waitForCanonicalRelayReceipt(t devtest.T, relay string, rpcClient apis.EthClient, txHash common.Hash, canonicalHash common.Hash) *types.Receipt {
	ctx, cancel := context.WithTimeout(t.Ctx(), 10*time.Second)
	defer cancel()
	ticker := time.NewTicker(100 * time.Millisecond)
	defer ticker.Stop()

	var lastHash common.Hash
	for {
		receipt, err := rpcClient.TransactionReceipt(ctx, txHash)
		if err == nil {
			lastHash = receipt.BlockHash
			if lastHash == canonicalHash {
				return receipt
			}
		} else if err != ethereum.NotFound {
			t.Require().NoError(err, "query %s transaction receipt", relay)
		}

		select {
		case <-ctx.Done():
			t.Require().NoError(ctx.Err(), "timed out waiting for %s canonical receipt %s (want block=%s, last block=%s)", relay, txHash, canonicalHash, lastHash)
		case <-ticker.C:
		}
	}
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

	// RelayPending subscribes before submission and uses executed flashblock
	// events to verify that rpc1 and rpc2 execute the transaction successfully in
	// the same pending block. After inclusion, both relays must expose the same
	// canonical receipt and discard the transaction from their next overlays.
	gt.Run("RelayPending", func(gt *testing.T) {
		t := devtest.ParallelT(gt)
		recipient := sys.Wallet.NewEOA(sys.L2EL).Address()
		rpc1Events, rpc1SubscriptionErr := subscribeRelayTransactions(t, sys.L2ELRPC1.Escape().UserRPC(), recipient)
		rpc2Events, rpc2SubscriptionErr := subscribeRelayTransactions(t, sys.L2ELRPC2.Escape().UserRPC(), recipient)

		sender := sys.FunderL2.NewFundedEOA(eth.OneEther)
		value := eth.OneHundredthEther
		waitForRelayExecution(t, rpc1Events, rpc1SubscriptionErr, "rpc1")
		waitForRelayExecution(t, rpc2Events, rpc2SubscriptionErr, "rpc2")

		planned := txplan.NewPlannedTx(sender.Plan(), txplan.WithTo(&recipient), txplan.WithValue(value))
		signed, err := planned.Signed.Eval(t.Ctx())
		t.Require().NoError(err, "sign relay propagation transaction")
		// A received flashblock can only be executed when its parent is already in
		// the relay's canonical database. Wait until both relays have the current
		// sequencer head, then broadcast immediately into the following pending
		// revisions. Merely observing an older header event is not sufficient: a
		// relay may subsequently fall behind while canonical payloads arrive.
		waitForCommonCanonicalBase(t, sys.L2EL.EthClient(), sys.L2ELRPC1.EthClient(), sys.L2ELRPC2.EthClient())
		_, err = planned.Submitted.Eval(t.Ctx())
		t.Require().NoError(err, "submit relay propagation transaction")
		txHash := signed.Hash()

		rpc1Event := waitForRelayTransaction(t, rpc1Events, rpc1SubscriptionErr, "rpc1", txHash)
		rpc2Event := waitForRelayTransaction(t, rpc2Events, rpc2SubscriptionErr, "rpc2", txHash)
		t.Require().NotNil(rpc1Event.Transaction.Receipt, "rpc1 must expose the executed flashblock receipt")
		t.Require().NotNil(rpc2Event.Transaction.Receipt, "rpc2 must expose the executed flashblock receipt")
		t.Require().Equal(txHash, rpc1Event.Transaction.Receipt.TxHash, "rpc1 receipt must identify the submitted transaction")
		t.Require().Equal(txHash, rpc2Event.Transaction.Receipt.TxHash, "rpc2 receipt must identify the submitted transaction")
		t.Require().Equal(uint64(1), rpc1Event.Transaction.Receipt.Status, "rpc1 flashblock execution must succeed")
		t.Require().Equal(uint64(1), rpc2Event.Transaction.Receipt.Status, "rpc2 flashblock execution must succeed")
		t.Require().NotNil(rpc1Event.Transaction.Receipt.BlockNumber, "rpc1 receipt must identify its pending block")
		t.Require().NotNil(rpc2Event.Transaction.Receipt.BlockNumber, "rpc2 receipt must identify its pending block")
		t.Require().Equal(rpc1Event.Transaction.Receipt.BlockNumber.Uint64(), rpc2Event.Transaction.Receipt.BlockNumber.Uint64(), "relays must expose the same pending block number")
		t.Require().Equal(rpc1Event.Transaction.Receipt.GasUsed, rpc2Event.Transaction.Receipt.GasUsed, "relays must produce the same pending execution result")

		sequencerReceipt, err := planned.Included.Eval(t.Ctx())
		t.Require().NoError(err, "relay propagation transaction must become canonical")
		t.Require().Equal(uint64(1), sequencerReceipt.Status, "relay propagation transaction must succeed")
		rpc1Receipt := waitForCanonicalRelayReceipt(t, "rpc1", sys.L2ELRPC1.EthClient(), txHash, sequencerReceipt.BlockHash)
		rpc2Receipt := waitForCanonicalRelayReceipt(t, "rpc2", sys.L2ELRPC2.EthClient(), txHash, sequencerReceipt.BlockHash)
		t.Require().Equal(sequencerReceipt.BlockHash, rpc1Receipt.BlockHash, "rpc1 canonical receipt must match the sequencer")
		t.Require().Equal(sequencerReceipt.BlockHash, rpc2Receipt.BlockHash, "rpc2 canonical receipt must match the sequencer")

		pendingContains := func(rpcClient interface {
			CallContext(ctx context.Context, result any, method string, args ...any) error
		}) (bool, error) {
			var block struct {
				Transactions []common.Hash `json:"transactions"`
			}
			if err := rpcClient.CallContext(t.Ctx(), &block, "eth_getBlockByNumber", "pending", false); err != nil {
				return false, err
			}
			for _, hash := range block.Transactions {
				if hash == txHash {
					return true, nil
				}
			}
			return false, nil
		}
		t.Require().Eventually(func() bool {
			rpc1Contains, err1 := pendingContains(sys.L2ELRPC1.EthClient().RPC())
			rpc2Contains, err2 := pendingContains(sys.L2ELRPC2.EthClient().RPC())
			return err1 == nil && err2 == nil && !rpc1Contains && !rpc2Contains
		}, 10*time.Second, 100*time.Millisecond, "canonical transaction must be removed from both relay pending overlays")
	})

	// EthSubscribeParamBoundaries uses its own WebSocket stream to rpc1, where
	// the custom flashblocks subscription API is enabled. Empty and invalid
	// params must yield a stable, decidable error rather than a valid subscription.
	gt.Run("EthSubscribeParamBoundaries", func(gt *testing.T) {
		t := devtest.ParallelT(gt)
		stream, err := client.NewRPC(t.Ctx(), t.Logger(), sys.L2ELRPC1.Escape().UserRPC(), client.WithLazyDial())
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
