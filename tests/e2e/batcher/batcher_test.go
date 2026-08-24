package batcher

import (
	"testing"
	"time"

	"github.com/ethereum/go-ethereum/core/types"

	batcherservice "github.com/ethereum-optimism/optimism/op-batcher/batcher"
	"github.com/ethereum-optimism/optimism/op-batcher/flags"
	"github.com/ethereum-optimism/optimism/op-devstack/devtest"
	"github.com/ethereum-optimism/optimism/op-devstack/presets"
	"github.com/ethereum-optimism/optimism/op-devstack/sysgo"
	"github.com/ethereum-optimism/optimism/op-service/eth"

	xcommon "github.com/okx/xlayer-reth/tests/common"
)

// TestBatcherBlobs verifies that the XLayer batcher publishes L2 data to its
// L1 inbox with blob transactions and that a validator derives the result.
func TestBatcherBlobs(gt *testing.T) {
	t := devtest.ParallelT(gt)
	testBatcherDA(t, flags.BlobsType, true)
}

// TestBatcherCalldata verifies that the default XLayer batcher publishes L2
// data to its L1 inbox with non-blob calldata transactions and that a validator
// derives the result.
func TestBatcherCalldata(gt *testing.T) {
	t := devtest.ParallelT(gt)
	testBatcherDA(t, flags.CalldataType, false)
}

func testBatcherDA(t devtest.T, daType flags.DataAvailabilityType, expectBlob bool) {
	cfg, err := xcommon.LoadXLayerConfig()
	t.Require().NoError(err, "harness config must load")
	t.Require().NoError(cfg.RequireExecutionBinary(), "RUST_BINARY_PATH_OP_RETH must point to a built XLayer reth binary")

	sys := presets.NewXLayer(t,
		presets.WithLocalContractSourcesAt(cfg.ForgeArtifactsDir()),
		presets.WithBatcherOption(func(_ sysgo.ComponentTarget, batcherCfg *batcherservice.CLIConfig) {
			batcherCfg.DataAvailabilityType = daType
		}),
	)

	l1Start, err := sys.L1EL.EthClient().InfoByLabel(t.Ctx(), eth.Unsafe)
	t.Require().NoError(err, "read L1 head before publishing the test batch")

	sender := sys.FunderL2.NewFundedEOA(eth.OneEther)
	recipient := sys.Wallet.NewEOA(sys.L2EL)
	receipt := xcommon.SendTransfer(t, sender, recipient.Address(), eth.OneHundredthEther)
	targetL2Block := receipt.BlockNumber.Uint64()
	batchInbox := sys.L2Chain.Escape().RollupConfig().BatchInboxAddress

	nextL1Block := l1Start.NumberU64() + 1
	foundBatch := false
	t.Require().Eventually(func() bool {
		head, err := sys.L1EL.EthClient().InfoByLabel(t.Ctx(), eth.Unsafe)
		if err != nil {
			return false
		}

		for nextL1Block <= head.NumberU64() {
			_, txs, err := sys.L1EL.EthClient().InfoAndTxsByNumber(t.Ctx(), nextL1Block)
			if err != nil {
				return false
			}
			for _, tx := range txs {
				isBlob := tx.Type() == types.BlobTxType
				isExpectedDA := isBlob == expectBlob && (expectBlob || len(tx.Data()) > 0)
				if tx.To() != nil && *tx.To() == batchInbox && isExpectedDA {
					foundBatch = true
					break
				}
			}
			nextL1Block++
		}
		if !foundBatch {
			return false
		}

		status, err := sys.L2CLRPC1.Escape().RollupAPI().SyncStatus(t.Ctx())
		return err == nil && status.SafeL2.Number >= targetL2Block
	}, 30*time.Second, 200*time.Millisecond,
		"batcher must publish the configured L1 inbox transaction and advance the validator safe head")
}
