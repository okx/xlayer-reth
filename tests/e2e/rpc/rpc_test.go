package rpc

import (
	"context"
	"math/big"
	"testing"

	"github.com/ethereum/go-ethereum"
	"github.com/ethereum/go-ethereum/common"

	xcommon "github.com/okx/xlayer-reth/tests/common"
)

// testContext derives a per-test context bounded by the configured hard timeout.
func testContext(sys *xcommon.SingleChainSystem) (context.Context, context.CancelFunc) {
	return context.WithTimeout(sys.DT.Ctx(), sys.Config.PerTestTimeout)
}

// Ports test_send_transaction: submit a native transfer and confirm it mines with
// a non-zero hash.
func TestSendTransaction(t *testing.T) {
	sys := xcommon.StartSingleChain(t)
	ctx, cancel := testContext(sys)
	defer cancel()

	tip, feeCap, err := sys.Seq.SuggestFeeCaps(ctx)
	if err != nil {
		t.Fatalf("fee caps: %v", err)
	}
	to := common.HexToAddress("0x3C44CdDdB6a900fa2b585dd299e03d12FA4293BC")
	hash, err := sys.Sender.TransferValue(ctx, sys.Seq, to, xcommon.OneEther, tip, feeCap)
	if err != nil {
		t.Fatalf("transfer: %v", err)
	}
	if (hash == common.Hash{}) {
		t.Fatal("expected a non-zero transaction hash")
	}
	if _, err := sys.Seq.WaitForTxMined(ctx, hash); err != nil {
		t.Fatalf("await mined: %v", err)
	}
}

// Ports test_ethereum_basic_rpc: the basic read RPCs against a live node.
func TestEthereumBasicRPC(t *testing.T) {
	sys := xcommon.StartSingleChain(t)
	ctx, cancel := testContext(sys)
	defer cancel()

	if id, err := sys.Seq.ChainID(ctx); err != nil || id.Sign() <= 0 {
		t.Fatalf("chainId: id=%v err=%v", id, err)
	}
	if bal, err := sys.Seq.BalanceAt(ctx, sys.Sender.Address); err != nil || bal.Sign() <= 0 {
		t.Fatalf("getBalance: bal=%v err=%v", bal, err)
	}
	if _, err := sys.Seq.CodeAt(ctx, sys.Sender.Address); err != nil {
		t.Fatalf("getCode: %v", err)
	}
	if _, err := sys.Seq.NonceAt(ctx, sys.Sender.Address); err != nil {
		t.Fatalf("getTransactionCount: %v", err)
	}
	if gp, err := sys.Seq.SuggestGasPrice(ctx); err != nil || gp.Sign() < 0 {
		t.Fatalf("gasPrice: gp=%v err=%v", gp, err)
	}
	if _, err := sys.Seq.StorageAt(ctx, sys.Sender.Address, common.Hash{}); err != nil {
		t.Fatalf("getStorageAt: %v", err)
	}
	if _, err := sys.Seq.BlockNumber(ctx); err != nil {
		t.Fatalf("blockNumber: %v", err)
	}
}

// Ports test_debug_trace_rpc: trace a block by hash and number, and a transaction.
func TestDebugTraceRPC(t *testing.T) {
	sys := xcommon.StartSingleChain(t)
	ctx, cancel := testContext(sys)
	defer cancel()

	if _, err := sys.Seq.WaitForBlocks(ctx, 3); err != nil {
		t.Fatalf("wait blocks: %v", err)
	}
	block, err := sys.Seq.BlockByNumber(ctx, nil)
	if err != nil {
		t.Fatalf("latest block: %v", err)
	}
	if _, err := sys.Seq.DebugTraceBlockByHash(ctx, block.Hash()); err != nil {
		t.Fatalf("traceBlockByHash: %v", err)
	}
	if _, err := sys.Seq.DebugTraceBlockByNumber(ctx, block.NumberU64()); err != nil {
		t.Fatalf("traceBlockByNumber: %v", err)
	}
	if txs := block.Transactions(); len(txs) > 0 {
		if _, err := sys.Seq.DebugTraceTransaction(ctx, txs[0].Hash()); err != nil {
			t.Fatalf("traceTransaction: %v", err)
		}
	}
}

// Ports test_eth_block_rpc: block queries plus block receipts by number and hash.
func TestEthBlockRPC(t *testing.T) {
	sys := xcommon.StartSingleChain(t)
	ctx, cancel := testContext(sys)
	defer cancel()

	n, err := sys.Seq.WaitForBlocks(ctx, 1)
	if err != nil {
		t.Fatalf("wait blocks: %v", err)
	}
	block, err := sys.Seq.BlockByNumber(ctx, new(big.Int).SetUint64(n))
	if err != nil {
		t.Fatalf("block by number: %v", err)
	}
	if _, err := sys.Seq.BlockReceiptsByNumber(ctx, "0x"+big.NewInt(int64(n)).Text(16)); err != nil {
		t.Fatalf("blockReceipts by number: %v", err)
	}
	if _, err := sys.Seq.BlockReceiptsByHash(ctx, block.Hash()); err != nil {
		t.Fatalf("blockReceipts by hash: %v", err)
	}
}

// Ports test_eth_transaction_rpc: gas estimate for a transfer equals 21000, an
// eth_call to a deployed contract's getValue, and receipt/tx lookups.
func TestEthTransactionRPC(t *testing.T) {
	sys := xcommon.StartSingleChain(t)
	ctx, cancel := testContext(sys)
	defer cancel()

	to := common.HexToAddress("0x3C44CdDdB6a900fa2b585dd299e03d12FA4293BC")
	gas, err := sys.Seq.EstimateGas(ctx, ethereum.CallMsg{From: sys.Sender.Address, To: &to, Value: big.NewInt(1)})
	if err != nil {
		t.Fatalf("estimateGas: %v", err)
	}
	if gas != 21000 {
		t.Fatalf("expected 21000 gas for a simple transfer, got %d", gas)
	}

	tip, feeCap, err := sys.Seq.SuggestFeeCaps(ctx)
	if err != nil {
		t.Fatalf("fee caps: %v", err)
	}
	fixtures, err := sys.Sender.DeployStandardFixtures(ctx, sys.Seq, 3_000_000, feeCap)
	if err != nil {
		t.Fatalf("deploy fixtures: %v", err)
	}
	if _, err := sys.Seq.CallContract(ctx, ethereum.CallMsg{To: &fixtures.ContractC, Data: xcommon.EncodeGetValue()}); err != nil {
		t.Fatalf("eth_call getValue: %v", err)
	}

	hash, err := sys.Sender.TransferValue(ctx, sys.Seq, to, big.NewInt(1), tip, feeCap)
	if err != nil {
		t.Fatalf("transfer: %v", err)
	}
	receipt, err := sys.Seq.WaitForTxMined(ctx, hash)
	if err != nil {
		t.Fatalf("await mined: %v", err)
	}
	tx, _, err := sys.Seq.TransactionByHash(ctx, hash)
	if err != nil {
		t.Fatalf("getTransactionByHash: %v", err)
	}
	if tx.Hash() != receipt.TxHash {
		t.Fatalf("tx/receipt hash mismatch: %s vs %s", tx.Hash().Hex(), receipt.TxHash.Hex())
	}
}

// Ports test_eth_logs_rpc: eth_getLogs over a recent block range returns without error.
func TestEthLogsRPC(t *testing.T) {
	sys := xcommon.StartSingleChain(t)
	ctx, cancel := testContext(sys)
	defer cancel()

	n, err := sys.Seq.WaitForBlocks(ctx, 1)
	if err != nil {
		t.Fatalf("wait blocks: %v", err)
	}
	q := ethereum.FilterQuery{FromBlock: new(big.Int).SetUint64(n), ToBlock: new(big.Int).SetUint64(n)}
	if _, err := sys.Seq.FilterLogs(ctx, q); err != nil {
		t.Fatalf("getLogs: %v", err)
	}
}

// Ports test_eth_get_logs_by_block_hash: filtering a known block by an
// unrelated address must return an empty result, not a "block not found" error.
func TestEthGetLogsByBlockHash(t *testing.T) {
	sys := xcommon.StartSingleChain(t)
	ctx, cancel := testContext(sys)
	defer cancel()

	n, err := sys.Seq.WaitForBlocks(ctx, 1)
	if err != nil {
		t.Fatalf("wait blocks: %v", err)
	}
	block, err := sys.Seq.BlockByNumber(ctx, new(big.Int).SetUint64(n))
	if err != nil {
		t.Fatalf("block by number: %v", err)
	}
	unrelated := common.HexToAddress("0x000000000000000000000000000000000000dEaD")
	logs, err := sys.Seq.LogsByBlockHash(ctx, block.Hash(), unrelated)
	if err != nil {
		t.Fatalf("getLogs by blockHash: %v", err)
	}
	if len(logs) != 0 {
		t.Fatalf("expected no logs for an unrelated address, got %d", len(logs))
	}
}

// Ports test_txpool_rpc: txpool_content and txpool_status respond without a
// process-level error.
func TestTxpoolRPC(t *testing.T) {
	sys := xcommon.StartSingleChain(t)
	ctx, cancel := testContext(sys)
	defer cancel()

	if _, err := sys.Seq.TxpoolContent(ctx); err != nil {
		t.Fatalf("txpool_content: %v", err)
	}
	if _, err := sys.Seq.TxpoolStatus(ctx); err != nil {
		t.Fatalf("txpool_status: %v", err)
	}
}

// Ports test_new_transaction_types: an EIP-1559 transfer reports type 2, a
// successful status, and exactly 21000 gas used.
func TestNewTransactionTypes(t *testing.T) {
	sys := xcommon.StartSingleChain(t)
	ctx, cancel := testContext(sys)
	defer cancel()

	tip, feeCap, err := sys.Seq.SuggestFeeCaps(ctx)
	if err != nil {
		t.Fatalf("fee caps: %v", err)
	}
	to := common.HexToAddress("0xAed6f7a2C1c9C4E2f7B3f2b0e9F0eE6B7A2c9Cc9")
	hash, err := sys.Sender.TransferValue(ctx, sys.Seq, to, big.NewInt(1), tip, feeCap)
	if err != nil {
		t.Fatalf("eip-1559 transfer: %v", err)
	}
	receipt, err := sys.Seq.WaitForTxMined(ctx, hash)
	if err != nil {
		t.Fatalf("await mined: %v", err)
	}
	if receipt.Type != 2 {
		t.Fatalf("expected dynamic-fee tx type 2, got %d", receipt.Type)
	}
	if receipt.GasUsed != 21000 {
		t.Fatalf("expected 21000 gas used, got %d", receipt.GasUsed)
	}
}
