package common

import (
	"context"
	"time"

	"github.com/ethereum/go-ethereum"
	"github.com/ethereum/go-ethereum/common"
	"github.com/ethereum/go-ethereum/common/hexutil"
	"github.com/ethereum/go-ethereum/core/types"
	"github.com/ethereum/go-ethereum/rpc"
	"github.com/lmittmann/w3"

	"github.com/ethereum-optimism/optimism/op-devstack/devtest"
	"github.com/ethereum-optimism/optimism/op-devstack/dsl"
	"github.com/ethereum-optimism/optimism/op-devstack/presets"
	"github.com/ethereum-optimism/optimism/op-service/apis"
	"github.com/ethereum-optimism/optimism/op-service/eth"
	"github.com/ethereum-optimism/optimism/op-service/txplan"
)

const receiptPollInterval = 200 * time.Millisecond

// SystemConfigAddress resolves and validates the SystemConfig referenced by the
// XLayer portal deployed for sys.
func SystemConfigAddress(t devtest.T, sys *presets.XLayer) common.Address {
	portal := sys.L2Chain.DepositContractAddr()
	fn := w3.MustNewFunc("systemConfig()", "address")
	data, err := fn.EncodeArgs()
	t.Require().NoError(err, "encode portal.systemConfig()")
	out, err := sys.L1EL.EthClient().Call(t.Ctx(), ethereum.CallMsg{To: &portal, Data: data}, rpc.LatestBlockNumber)
	t.Require().NoError(err, "call portal.systemConfig()")

	var addr common.Address
	t.Require().NoError(fn.DecodeReturns(out, &addr), "decode portal.systemConfig()")
	t.Require().NotEqual(common.Address{}, addr, "portal must reference a SystemConfig")
	return addr
}

// SendTransfer submits a native-value transfer and requires successful
// canonical inclusion before returning its receipt.
func SendTransfer(t devtest.T, sender *dsl.EOA, to common.Address, value eth.ETH) *types.Receipt {
	ptx := txplan.NewPlannedTx(sender.Plan(), txplan.WithTo(&to), txplan.WithValue(value))
	receipt, err := ptx.Included.Eval(t.Ctx())
	t.Require().NoError(err, "transfer must be included")
	t.Require().Equal(types.ReceiptStatusSuccessful, receipt.Status, "transfer must succeed")
	return receipt
}

// WaitForReceipt polls client until txHash is available or timeout expires.
func WaitForReceipt(t devtest.T, client apis.EthClient, txHash common.Hash, timeout time.Duration) *types.Receipt {
	ctx, cancel := context.WithTimeout(t.Ctx(), timeout)
	defer cancel()

	ticker := time.NewTicker(receiptPollInterval)
	defer ticker.Stop()
	for {
		receipt, err := client.TransactionReceipt(ctx, txHash)
		if err == nil {
			return receipt
		}
		if err != ethereum.NotFound {
			t.Require().NoError(err, "query transaction receipt")
		}
		select {
		case <-ctx.Done():
			t.Require().NoError(ctx.Err(), "timed out waiting for transaction receipt %s", txHash)
		case <-ticker.C:
		}
	}
}

// BlockRootAndHash returns the state root and block hash at number.
func BlockRootAndHash(t devtest.T, client apis.EthClient, number uint64) (common.Hash, common.Hash) {
	var block struct {
		StateRoot common.Hash `json:"stateRoot"`
		Hash      common.Hash `json:"hash"`
	}
	err := client.RPC().CallContext(t.Ctx(), &block, "eth_getBlockByNumber", hexutil.EncodeUint64(number), false)
	t.Require().NoError(err, "read block %d", number)
	return block.StateRoot, block.Hash
}
