package flashblocks

import (
	"context"
	"math/big"
	"testing"

	"github.com/ethereum/go-ethereum/common"

	xcommon "github.com/okx/xlayer-reth/tests/common"
)

// Ports fb_smoke_test: verify the flashblock-supported RPCs honor the pending tag
// on the producer node. A native transfer is submitted, then pending-tagged reads
// and the standard balance/nonce/tx lookups are exercised against the FB node.
func TestFlashblocksSmoke(t *testing.T) {
	sys := xcommon.StartFlashblocks(t)
	ctx, cancel := context.WithTimeout(sys.DT.Ctx(), sys.Config.PerTestTimeout)
	defer cancel()

	if _, err := sys.Client.WaitForBlocks(ctx, 1); err != nil {
		t.Fatalf("wait blocks: %v", err)
	}

	// Pending-tagged block transaction count must be answerable without error.
	if _, err := sys.Client.BlockTransactionCount(ctx, xcommon.BlockPending); err != nil {
		t.Fatalf("pending block tx count: %v", err)
	}

	tip, feeCap, err := sys.Client.SuggestFeeCaps(ctx)
	if err != nil {
		t.Fatalf("fee caps: %v", err)
	}
	to := common.HexToAddress("0x3C44CdDdB6a900fa2b585dd299e03d12FA4293BC")
	hash, err := sys.Producer.TransferValue(ctx, sys.Client, to, big.NewInt(1), tip, feeCap)
	if err != nil {
		t.Fatalf("transfer: %v", err)
	}
	receipt, err := sys.Client.WaitForTxMined(ctx, hash)
	if err != nil {
		t.Fatalf("await mined: %v", err)
	}

	if tx, _, err := sys.Client.TransactionByHash(ctx, hash); err != nil || tx == nil {
		t.Fatalf("getTransactionByHash: tx=%v err=%v", tx, err)
	}
	if receipt.TxHash != hash {
		t.Fatalf("receipt tx hash mismatch: %s vs %s", receipt.TxHash.Hex(), hash.Hex())
	}
	if bal, err := sys.Client.BalanceAt(ctx, sys.Producer.Address); err != nil || bal.Sign() < 0 {
		t.Fatalf("getBalance: bal=%v err=%v", bal, err)
	}
	if _, err := sys.Client.NonceAt(ctx, sys.Producer.Address); err != nil {
		t.Fatalf("getTransactionCount: %v", err)
	}
	if _, err := sys.Client.BlockByNumber(ctx, nil); err != nil {
		t.Fatalf("getBlockByNumber: %v", err)
	}
}
