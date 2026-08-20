package gasless

import (
	"context"
	"encoding/json"
	"fmt"
	"math/big"
	"testing"
	"time"

	"github.com/ethereum/go-ethereum/common"
	"github.com/ethereum/go-ethereum/common/hexutil"
	"github.com/ethereum/go-ethereum/core/types"
	"github.com/ethereum/go-ethereum/crypto"

	"github.com/ethereum-optimism/optimism/op-chain-ops/devkeys"
	"github.com/ethereum-optimism/optimism/op-devstack/devtest"
	"github.com/ethereum-optimism/optimism/op-devstack/dsl"
	"github.com/ethereum-optimism/optimism/op-devstack/presets"
	"github.com/ethereum-optimism/optimism/op-devstack/sysgo"
	"github.com/ethereum-optimism/optimism/op-e2e/e2eutils/intentbuilder"
	"github.com/ethereum-optimism/optimism/op-service/apis"
	"github.com/ethereum-optimism/optimism/op-service/eth"
	"github.com/ethereum-optimism/optimism/op-service/txplan"

	xcommon "github.com/okx/xlayer-reth/tests/common"
)

// gaslessGasLimit is the per-target gas allowance registered for a whitelisted
// gasless target. It matches the whitelist implementation's default maxGasLimit.
const gaslessGasLimit uint64 = 16_777_216

// gaslessTxGasLimit is the fixed gas limit for a zero-priced gasless transfer,
// large enough for a native transfer carrying a short calldata probe.
const gaslessTxGasLimit uint64 = 50_000

// gaslessOwnerTxGasLimit avoids estimating later owner calls against canonical
// state where the preceding initialize transaction is not visible yet.
const gaslessOwnerTxGasLimit uint64 = 200_000

// gaslessOwnerKey is prefunded in the L2 genesis. The faucet owns UserKey(10_000),
// so this dedicated key cannot conflict with the faucet transaction manager.
const gaslessOwnerKey devkeys.UserKey = 10_001

// gaslessReceiptTimeout allows ample time for a transaction to cross multiple
// block-production and L1-derivation cycles while still failing quickly when a
// sequencer or validator stops making progress.
const gaslessReceiptTimeout = 20 * time.Second

// gaslessProbeData is a 4-byte calldata prefix carried by gasless transactions so
// the whitelist's calldata-length guard (which rejects anything shorter than a
// 4-byte selector) passes. An empty input can never be gasless.
var gaslessProbeData = common.FromHex("0xdeadbeef")

func xlayerBinaryOpts(t devtest.T, cfg *xcommon.XLayerConfig) []presets.Option {
	t.Require().NoError(cfg.RequireExecutionBinary(), "RUST_BINARY_PATH_OP_RETH must point to a built XLayer reth binary")

	return []presets.Option{
		presets.WithLocalContractSourcesAt(cfg.ForgeArtifactsDir()),
		presets.WithDeployerOptions(func(t devtest.T, keys devkeys.Keys, builder intentbuilder.Builder) {
			owner, err := keys.Address(gaslessOwnerKey)
			t.Require().NoError(err, "derive gasless owner address")
			for _, l2 := range builder.L2s() {
				l2.WithPrefundedAccount(owner, *eth.OneEther.ToU256())
			}
		}),
		// Enable the XLayer gasless transaction pool. With gasless enabled the pool
		// admits zero-priced (maxFeePerGas == maxPriorityFeePerGas == 0) whitelisted
		// transactions and assigns them a mock ordering tip, so no base-fee or
		// minimum-priority-fee tuning is needed.
		presets.WithOpRethOption(sysgo.OpRethWithExtraArgs("--rollup.allow-gasless")),
	}
}

func newXLayerGasless(gt *testing.T) (devtest.T, *presets.XLayer, *xcommon.XLayerConfig) {
	t := devtest.ParallelT(gt)
	cfg, err := xcommon.LoadXLayerConfig()
	t.Require().NoError(err, "harness config must load")
	return t, presets.NewXLayer(t, xlayerBinaryOpts(t, cfg)...), cfg
}

// funcSelector returns the 4-byte function selector for the given Solidity
// signature.
func funcSelector(signature string) []byte {
	return crypto.Keccak256([]byte(signature))[:4]
}

// packInitialize encodes initialize(address).
func packInitialize(owner common.Address) []byte {
	return append(funcSelector("initialize(address)"), common.LeftPadBytes(owner.Bytes(), 32)...)
}

// packSetGaslessEnabled encodes setGaslessEnabled(bool).
func packSetGaslessEnabled(enabled bool) []byte {
	flag := byte(0)
	if enabled {
		flag = 1
	}
	return append(funcSelector("setGaslessEnabled(bool)"), common.LeftPadBytes([]byte{flag}, 32)...)
}

// packSetFullyGaslessTarget encodes setFullyGaslessTarget(address,bool,uint64).
func packSetFullyGaslessTarget(target common.Address, allowed bool, gasLimit uint64) []byte {
	flag := byte(0)
	if allowed {
		flag = 1
	}
	data := funcSelector("setFullyGaslessTarget(address,bool,uint64)")
	data = append(data, common.LeftPadBytes(target.Bytes(), 32)...)
	data = append(data, common.LeftPadBytes([]byte{flag}, 32)...)
	data = append(data, common.LeftPadBytes(new(big.Int).SetUint64(gasLimit).Bytes(), 32)...)
	return data
}

// gaslessWhitelist is the fixed address the execution client reads gasless rules
// from; the runtime is installed there at genesis (see the sysgo gasless
// predeploys), so the tests own their precondition by initializing and enabling
// it directly rather than depending on a devnet bootstrap step.
func gaslessWhitelist() common.Address {
	return sysgo.XLayerGaslessWhitelistProxy
}

// gaslessOwner returns the dedicated account that xlayerBinaryOpts prefunds in
// the L2 genesis, avoiding a faucet transaction during test setup.
func gaslessOwner(t devtest.T, sys *presets.XLayer) *dsl.EOA {
	priv := sys.L2Chain.Escape().Keys().Secret(gaslessOwnerKey)
	return dsl.NewKey(t, priv).User(sys.L2EL)
}

// enableGaslessWhitelist initializes the genesis-installed whitelist with owner
// as its owner, enables gasless, and registers target as a fully gasless target.
// The three ordinary fee-paying transactions are broadcast with consecutive
// nonces, then verified after the final transaction is included. This lets them
// share one canonical block without losing per-call revert detection.
func enableGaslessWhitelist(t devtest.T, owner *dsl.EOA, target common.Address) {
	whitelist := gaslessWhitelist()
	baseNonce := owner.PendingNonce()
	calls := [][]byte{
		packInitialize(owner.Address()),
		packSetGaslessEnabled(true),
		packSetFullyGaslessTarget(target, true, gaslessGasLimit),
	}

	plans := make([]*txplan.PlannedTx, len(calls))
	for i, data := range calls {
		plans[i] = txplan.NewPlannedTx(
			owner.Plan(),
			txplan.WithStaticNonce(baseNonce+uint64(i)),
			txplan.WithTo(&whitelist),
			txplan.WithData(data),
			txplan.WithGasLimit(gaslessOwnerTxGasLimit),
		)
		_, err := plans[i].Submitted.Eval(t.Ctx())
		t.Require().NoErrorf(err, "submit whitelist transaction %d", i)
	}

	// Inclusion of the highest nonce guarantees the preceding nonces were also
	// consumed. Verify every receipt afterward because a reverted transaction
	// consumes its nonce too, and checking only the last status would miss that.
	_, err := plans[len(plans)-1].Submitted.Eval(t.Ctx())
	t.Require().NoErrorf(err, "final whitelist transaction must be submitted")
	_, err = plans[len(plans)-1].Success.Eval(t.Ctx())
	t.Require().NoError(err, "final whitelist transaction must succeed")
}

// chainID reads the L2 chain id from the sequencer.
func chainID(t devtest.T, client apis.EthClient) *big.Int {
	var id hexutil.Big
	err := client.RPC().CallContext(t.Ctx(), &id, "eth_chainId")
	t.Require().NoError(err, "read chain id")
	return (*big.Int)(&id)
}

// sendGaslessTransfers signs and submits zero-priced transfers with consecutive
// nonces. Submitting a batch without waiting between transactions lets replay
// tests place multiple gasless transactions in one canonical block.
func sendGaslessTransfers(t devtest.T, client apis.EthClient, sender *dsl.EOA, to common.Address, values ...*big.Int) []common.Hash {
	id := chainID(t, client)

	var nonce hexutil.Uint64
	err := client.RPC().CallContext(t.Ctx(), &nonce, "eth_getTransactionCount", sender.Address(), "pending")
	t.Require().NoError(err, "read sender nonce")

	hashes := make([]common.Hash, len(values))
	for i, value := range values {
		tx := types.NewTx(&types.DynamicFeeTx{
			ChainID:   id,
			Nonce:     uint64(nonce) + uint64(i),
			GasTipCap: big.NewInt(0),
			GasFeeCap: big.NewInt(0),
			Gas:       gaslessTxGasLimit,
			To:        &to,
			Value:     value,
			Data:      gaslessProbeData,
		})
		signed, err := types.SignTx(tx, types.LatestSignerForChainID(id), sender.Key().Priv())
		t.Require().NoError(err, "sign gasless tx %d", i)
		raw, err := signed.MarshalBinary()
		t.Require().NoError(err, "encode gasless tx %d", i)

		err = client.RPC().CallContext(t.Ctx(), &hashes[i], "eth_sendRawTransaction", hexutil.Encode(raw))
		t.Require().NoError(err, "zero-priced gasless transfer %d must be accepted by the node", i)
	}
	return hashes
}

// sendGaslessTransfer submits one gasless transfer.
func sendGaslessTransfer(t devtest.T, client apis.EthClient, sender *dsl.EOA, to common.Address, value *big.Int) common.Hash {
	return sendGaslessTransfers(t, client, sender, to, value)[0]
}

// gaslessReceipt is the minimal receipt shape the gasless assertions need.
type gaslessReceipt struct {
	Status      hexutil.Uint64 `json:"status"`
	BlockNumber hexutil.Uint64 `json:"blockNumber"`
	BlockHash   common.Hash    `json:"blockHash"`
}

// waitForGaslessReceipt polls until endpoint reports a successful receipt for
// hash and returns it. For the validator this proves it imported (and validated)
// the block the sequencer produced.
func waitForGaslessReceipt(t devtest.T, client apis.EthClient, hash common.Hash) gaslessReceipt {
	ctx, cancel := context.WithTimeout(t.Ctx(), gaslessReceiptTimeout)
	defer cancel()

	for {
		var receipt *gaslessReceipt
		err := client.RPC().CallContext(ctx, &receipt, "eth_getTransactionReceipt", hash)
		if ctx.Err() != nil {
			t.Require().NoError(ctx.Err(), "timed out waiting for gasless tx %s to be mined", hash)
		}
		t.Require().NoError(err, "poll gasless receipt")
		if receipt != nil {
			t.Require().Equal(hexutil.Uint64(1), receipt.Status, "gasless tx must be mined successfully")
			return *receipt
		}

		select {
		case <-ctx.Done():
			t.Require().NoError(ctx.Err(), "timed out waiting for gasless tx %s to be mined", hash)
		case <-time.After(200 * time.Millisecond):
		}
	}
}


// assertNodesAgree requires the sequencer and validator to compute the same
// stateRoot and block hash for blockNumber — the core consensus-uniformity check
// that both applied identical gasless fee/gas accounting.
func assertNodesAgree(t devtest.T, seq, validator apis.EthClient, blockNumber uint64) {
	seqRoot, seqHash := xcommon.BlockRootAndHash(t, seq, blockNumber)
	valRoot, valHash := xcommon.BlockRootAndHash(t, validator, blockNumber)
	t.Require().Equal(seqRoot, valRoot, "sequencer and validator must agree on stateRoot for gasless block %d", blockNumber)
	t.Require().Equal(seqHash, valHash, "sequencer and validator must agree on hash for gasless block %d", blockNumber)
}

// zeroFeeGaslessCall builds a zero-priced call object whose target is whitelisted
// and whose input carries the calldata probe, so it is detected as gasless on the
// RPC re-execution path.
func zeroFeeGaslessCall(from, to common.Address) map[string]any {
	return map[string]any{
		"from":                 from.Hex(),
		"to":                   to.Hex(),
		"value":                "0x1",
		"input":                hexutil.Encode(gaslessProbeData),
		"gas":                  hexutil.EncodeUint64(gaslessTxGasLimit),
		"maxFeePerGas":         "0x0",
		"maxPriorityFeePerGas": "0x0",
	}
}

// TestGasless exercises the XLayer gasless (zero-priced) transaction path. All
// cases share a single devnet and one immutable whitelist setup, then run as
// parallel subtests with independently funded senders. Sharing the expensive
// topology and setup keeps the cases isolated without serializing their waits.
func TestGasless(gt *testing.T) {
	start := time.Now()
	t, sys, _ := newXLayerGasless(gt)

	seq := sys.L2EL.EthClient()
	validator := sys.L2ELRPC1.EthClient()
	// One-time precondition shared by every subtest: enable the genesis-installed
	// whitelist and register the transfer target with the prefunded owner.
	owner := gaslessOwner(t, sys)
	target := sys.Wallet.NewEOA(sys.L2EL).Address()
	enableGaslessWhitelist(t, owner, target)
	now := time.Now()
	fmt.Printf("TestGasless setup took %s (started at %s)\n",
		now.Sub(start).Round(time.Second), start.Format(time.RFC3339))

	// ZeroPriceTransfer: the sequencer mines a zero-priced transfer, the validator
	// imports it at the same height and agrees on the post-execution state root,
	// and the sequencer keeps producing blocks so a second gasless transfer lands
	// in a later block the validator also follows.
	gt.Run("ZeroPriceTransfer", func(gt *testing.T) {
		t := devtest.ParallelT(gt)
		sender := sys.FunderL2.NewFundedEOA(eth.OneEther)
		hash1 := sendGaslessTransfer(t, seq, sender, target, big.NewInt(1))
		seqReceipt1 := waitForGaslessReceipt(t, seq, hash1)
		valReceipt1 := waitForGaslessReceipt(t, validator, hash1)
		t.Require().Equal(seqReceipt1.BlockNumber, valReceipt1.BlockNumber, "validator must import the gasless tx at the same block")
		assertNodesAgree(t, seq, validator, uint64(seqReceipt1.BlockNumber))

		hash2 := sendGaslessTransfer(t, seq, sender, target, big.NewInt(1))
		seqReceipt2 := waitForGaslessReceipt(t, seq, hash2)
		t.Require().Greater(uint64(seqReceipt2.BlockNumber), uint64(seqReceipt1.BlockNumber),
			"second gasless tx must land in a later block; the sequencer must keep producing blocks after a gasless block")
		valReceipt2 := waitForGaslessReceipt(t, validator, hash2)
		t.Require().Equal(seqReceipt2.BlockNumber, valReceipt2.BlockNumber, "validator must follow past the gasless blocks")
		assertNodesAgree(t, seq, validator, uint64(seqReceipt2.BlockNumber))
	})

	// DebugTrace: tracing the second of two gasless transactions in the same block
	// must replay the first with canonical gasless semantics instead of rejecting
	// it because its zero fee cap is below the block base fee.
	gt.Run("DebugTrace", func(gt *testing.T) {
		t := devtest.ParallelT(gt)
		sender := sys.FunderL2.NewFundedEOA(eth.OneEther)

		// A pair submitted immediately after a split pair's second receipt has a
		// fresh block interval in which to enter together. Keep the retry bounded:
		// failure means the harness could not establish the regression precondition.
		var hash common.Hash
		for attempt := 0; attempt < 3 && hash == (common.Hash{}); attempt++ {
			hashes := sendGaslessTransfers(t, seq, sender, target, big.NewInt(1), big.NewInt(1))
			first := waitForGaslessReceipt(t, seq, hashes[0])
			second := waitForGaslessReceipt(t, seq, hashes[1])
			if first.BlockHash == second.BlockHash {
				hash = hashes[1]
			}
		}
		t.Require().NotEqual(common.Hash{}, hash, "two consecutive gasless transactions must land in the same block")

		var trace map[string]json.RawMessage
		err := seq.RPC().CallContext(t.Ctx(), &trace, "debug_traceTransaction", hash)
		t.Require().NoError(err, "debug_traceTransaction must succeed for a gasless tx (no base-fee rejection)")
		_, hasStructLogs := trace["structLogs"]
		_, hasGas := trace["gas"]
		t.Require().True(hasStructLogs || hasGas, "unexpected debug_traceTransaction shape")
	})

	// TxRPCGasPriceIsZero: a mined gasless transaction reports a zero gas price via
	// eth_getTransactionByHash.
	gt.Run("TxRPCGasPriceIsZero", func(gt *testing.T) {
		t := devtest.ParallelT(gt)
		sender := sys.FunderL2.NewFundedEOA(eth.OneEther)
		hash := sendGaslessTransfer(t, seq, sender, target, big.NewInt(1))
		waitForGaslessReceipt(t, seq, hash)

		var tx map[string]json.RawMessage
		err := seq.RPC().CallContext(t.Ctx(), &tx, "eth_getTransactionByHash", hash)
		t.Require().NoError(err, "gasless tx must be retrievable by hash")
		var gasPrice string
		t.Require().NoError(json.Unmarshal(tx["gasPrice"], &gasPrice), "decode gasPrice")
		t.Require().Equal("0x0", gasPrice, "gasless tx must report gasPrice 0x0")
	})

	// EthCall: a zero-priced call to a whitelisted target executes on the
	// gasless-aware eth_call path rather than being rejected by the base-fee check.
	gt.Run("EthCall", func(gt *testing.T) {
		t := devtest.ParallelT(gt)
		sender := sys.FunderL2.NewFundedEOA(eth.OneEther)
		call := zeroFeeGaslessCall(sender.Address(), target)
		var result hexutil.Bytes
		err := seq.RPC().CallContext(t.Ctx(), &result, "eth_call", call, "latest")
		t.Require().NoError(err, "zero gas price gasless eth_call must execute")
	})

	// EthSimulateV1: eth_simulateV1 is gasless-aware for a zero-priced call bundle
	// targeting a whitelisted address.
	gt.Run("EthSimulateV1", func(gt *testing.T) {
		t := devtest.ParallelT(gt)
		sender := sys.FunderL2.NewFundedEOA(eth.OneEther)
		bundle := map[string]any{
			"blockStateCalls": []any{
				map[string]any{
					"calls": []any{zeroFeeGaslessCall(sender.Address(), target)},
				},
			},
		}
		var result json.RawMessage
		err := seq.RPC().CallContext(t.Ctx(), &result, "eth_simulateV1", bundle, "latest")
		t.Require().NoError(err, "eth_simulateV1 must accept a zero gas price gasless bundle")
	})
}
