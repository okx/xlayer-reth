package rpc

import (
	"encoding/json"
	"math/big"
	"testing"

	"github.com/ethereum/go-ethereum/common"
	"github.com/ethereum/go-ethereum/common/hexutil"
	"github.com/ethereum/go-ethereum/core/types"

	"github.com/ethereum-optimism/optimism/op-devstack/devtest"
	"github.com/ethereum-optimism/optimism/op-devstack/dsl"
	"github.com/ethereum-optimism/optimism/op-devstack/presets"
	"github.com/ethereum-optimism/optimism/op-service/eth"
	"github.com/ethereum-optimism/optimism/op-service/txplan"
)

// callRPC issues a raw JSON-RPC call against the sequencer EL and fails the test
// if the node returns a transport/handler error. Individual scenarios decode the
// result into whatever shape they assert on.
func callRPC(t devtest.T, sys *presets.XLayer, out any, method string, args ...any) {
	err := sys.L2EL.EthClient().RPC().CallContext(t.Ctx(), out, method, args...)
	t.Require().NoErrorf(err, "rpc %s must not return a process-level error", method)
}

// sendTransfer submits a value transfer from the funded sender to the recipient
// and returns the inclusion receipt once it is canonical.
func sendTransfer(t devtest.T, sender *dsl.EOA, to common.Address, value eth.ETH) *types.Receipt {
	ptx := txplan.NewPlannedTx(sender.Plan(), txplan.WithTo(&to), txplan.WithValue(value))
	receipt, err := ptx.Included.Eval(t.Ctx())
	t.Require().NoError(err, "transfer must be included")
	t.Require().Equal(types.ReceiptStatusSuccessful, receipt.Status, "transfer must succeed")
	return receipt
}

func newXLayerRPC(gt *testing.T) (devtest.T, *presets.XLayer) {
	t := devtest.SerialT(gt)
	return t, presets.NewXLayer(t)
}

// TestSendTransaction submits a native transfer and checks the recipient balance
// reflects it at the inclusion block.
func TestSendTransaction(gt *testing.T) {
	t, sys := newXLayerRPC(gt)
	alice := sys.FunderL2.NewFundedEOA(eth.OneEther)
	bob := sys.Wallet.NewEOA(sys.L2EL)
	bobAddr := bob.Address()

	receipt := sendTransfer(t, alice, bobAddr, eth.OneHundredthEther)

	bal, err := sys.L2EL.EthClient().BalanceAt(t.Ctx(), bobAddr, receipt.BlockNumber)
	t.Require().NoError(err)
	t.Require().Equal(0, bal.Cmp(eth.OneHundredthEther.ToBig()), "recipient balance must equal the transferred value")
}

// TestEthereumBasicRPC exercises the read-only Ethereum JSON-RPC surface against
// a real XLayer sequencer: chainId, syncing, balance, code, block number, nonce,
// gas price and storage. They share one devnet because each case is cheap.
func TestEthereumBasicRPC(gt *testing.T) {
	t, sys := newXLayerRPC(gt)
	alice := sys.FunderL2.NewFundedEOA(eth.OneEther)
	addr := alice.Address()

	var chainID hexutil.Big
	callRPC(t, sys, &chainID, "eth_chainId")
	t.Require().Positive((*big.Int)(&chainID).Sign(), "chain id must be positive")

	var syncing json.RawMessage
	callRPC(t, sys, &syncing, "eth_syncing")
	t.Require().NotEmpty(syncing, "eth_syncing must return a result")

	var balance hexutil.Big
	callRPC(t, sys, &balance, "eth_getBalance", addr, "latest")

	var code hexutil.Bytes
	callRPC(t, sys, &code, "eth_getCode", addr, "latest")

	var blockNumber hexutil.Uint64
	callRPC(t, sys, &blockNumber, "eth_blockNumber")

	var nonce hexutil.Uint64
	callRPC(t, sys, &nonce, "eth_getTransactionCount", addr, "latest")

	var gasPrice hexutil.Big
	callRPC(t, sys, &gasPrice, "eth_gasPrice")

	var storage hexutil.Bytes
	callRPC(t, sys, &storage, "eth_getStorageAt", addr, "0x0", "latest")
}

// TestEthBlockRPC covers block-oriented queries by hash and number, plus block
// transaction counts, using a block that includes a real transfer.
func TestEthBlockRPC(gt *testing.T) {
	t, sys := newXLayerRPC(gt)
	alice := sys.FunderL2.NewFundedEOA(eth.OneEther)
	bob := sys.Wallet.NewEOA(sys.L2EL)
	bobAddr := bob.Address()
	receipt := sendTransfer(t, alice, bobAddr, eth.OneHundredthEther)
	blockNumHex := hexutil.EncodeBig(receipt.BlockNumber)

	var byNumber map[string]json.RawMessage
	callRPC(t, sys, &byNumber, "eth_getBlockByNumber", blockNumHex, true)
	t.Require().NotEmpty(byNumber["hash"], "block must have a hash")

	var byHash map[string]json.RawMessage
	callRPC(t, sys, &byHash, "eth_getBlockByHash", receipt.BlockHash, true)
	t.Require().NotEmpty(byHash["number"], "block must have a number")

	var countByNumber hexutil.Uint64
	callRPC(t, sys, &countByNumber, "eth_getBlockTransactionCountByNumber", blockNumHex)

	var countByHash hexutil.Uint64
	callRPC(t, sys, &countByHash, "eth_getBlockTransactionCountByHash", receipt.BlockHash)

	var receipts json.RawMessage
	callRPC(t, sys, &receipts, "eth_getBlockReceipts", blockNumHex)
	t.Require().NotEmpty(receipts, "block receipts must be returned")
}

// TestEthTransactionRPC covers transaction-oriented queries and gas estimation
// against a canonical transaction.
func TestEthTransactionRPC(gt *testing.T) {
	t, sys := newXLayerRPC(gt)
	alice := sys.FunderL2.NewFundedEOA(eth.OneEther)
	bob := sys.Wallet.NewEOA(sys.L2EL)
	bobAddr := bob.Address()
	receipt := sendTransfer(t, alice, bobAddr, eth.OneHundredthEther)

	var txByHash map[string]json.RawMessage
	callRPC(t, sys, &txByHash, "eth_getTransactionByHash", receipt.TxHash)
	t.Require().NotEmpty(txByHash["blockHash"], "canonical tx must reference its block")

	var receiptByHash map[string]json.RawMessage
	callRPC(t, sys, &receiptByHash, "eth_getTransactionReceipt", receipt.TxHash)
	t.Require().NotEmpty(receiptByHash["status"], "receipt must carry a status")

	call := map[string]any{"from": alice.Address().Hex(), "to": bobAddr.Hex(), "value": "0x1"}
	var estimate hexutil.Uint64
	callRPC(t, sys, &estimate, "eth_estimateGas", call)
	t.Require().Positive(uint64(estimate), "gas estimate must be non-zero")

	var callResult hexutil.Bytes
	callRPC(t, sys, &callResult, "eth_call", call, "latest")
}

// TestDebugTraceRPC checks the debug trace surface returns success semantics for
// a canonical transaction and its block.
func TestDebugTraceRPC(gt *testing.T) {
	t, sys := newXLayerRPC(gt)
	alice := sys.FunderL2.NewFundedEOA(eth.OneEther)
	bob := sys.Wallet.NewEOA(sys.L2EL)
	bobAddr := bob.Address()
	receipt := sendTransfer(t, alice, bobAddr, eth.OneHundredthEther)
	blockNumHex := hexutil.EncodeBig(receipt.BlockNumber)

	var traceTx json.RawMessage
	callRPC(t, sys, &traceTx, "debug_traceTransaction", receipt.TxHash)
	t.Require().NotEmpty(traceTx, "debug_traceTransaction must return a trace")

	var traceByHash json.RawMessage
	callRPC(t, sys, &traceByHash, "debug_traceBlockByHash", receipt.BlockHash)
	t.Require().NotEmpty(traceByHash, "debug_traceBlockByHash must return traces")

	var traceByNumber json.RawMessage
	callRPC(t, sys, &traceByNumber, "debug_traceBlockByNumber", blockNumHex)
	t.Require().NotEmpty(traceByNumber, "debug_traceBlockByNumber must return traces")
}

// TestEthLogsRPC covers eth_getLogs by range and by block hash, asserting the
// node returns a well-formed (possibly empty) result rather than an error.
func TestEthLogsRPC(gt *testing.T) {
	t, sys := newXLayerRPC(gt)
	alice := sys.FunderL2.NewFundedEOA(eth.OneEther)
	bob := sys.Wallet.NewEOA(sys.L2EL)
	bobAddr := bob.Address()
	receipt := sendTransfer(t, alice, bobAddr, eth.OneHundredthEther)

	var byRange []json.RawMessage
	callRPC(t, sys, &byRange, "eth_getLogs", map[string]any{"fromBlock": "earliest", "toBlock": "latest"})

	var byBlockHash []json.RawMessage
	callRPC(t, sys, &byBlockHash, "eth_getLogs", map[string]any{"blockHash": receipt.BlockHash.Hex()})
}

// TestTxpoolRPC covers the txpool inspection surface.
func TestTxpoolRPC(gt *testing.T) {
	t, sys := newXLayerRPC(gt)

	var content json.RawMessage
	callRPC(t, sys, &content, "txpool_content")
	t.Require().NotEmpty(content, "txpool_content must return a result")

	var status map[string]hexutil.Uint64
	callRPC(t, sys, &status, "txpool_status")
	_, hasPending := status["pending"]
	t.Require().True(hasPending, "txpool_status must report a pending count")
}

// TestNewTransactionTypes covers the typed-transaction RPC surface. It asserts
// the fee-history endpoint (EIP-1559 support) responds; the remaining EIP-cased
// variants from the Rust suite build on the same submission path and are tracked
// as follow-up cases once devnet execution is wired.
func TestNewTransactionTypes(gt *testing.T) {
	t, sys := newXLayerRPC(gt)

	var feeHistory json.RawMessage
	callRPC(t, sys, &feeHistory, "eth_feeHistory", "0x4", "latest", []float64{25, 75})
	t.Require().NotEmpty(feeHistory, "eth_feeHistory must return a result")
	// TODO: port the remaining typed-transaction cases (EIP-1559 contract call,
	// EIP-2930 access list, EIP-3198 basefee opcode, EIP-3529 refunds, EIP-4844
	// blob fields) from the Rust suite; they require devnet execution to assert
	// on-chain effects and are exercised once the harness runs live.
}
