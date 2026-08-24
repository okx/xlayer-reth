package rpc

import (
	"encoding/json"
	"math/big"
	"testing"

	"github.com/ethereum/go-ethereum"
	"github.com/ethereum/go-ethereum/common"
	"github.com/ethereum/go-ethereum/common/hexutil"
	gethrpc "github.com/ethereum/go-ethereum/rpc"
	"github.com/lmittmann/w3"

	"github.com/ethereum-optimism/optimism/op-devstack/devtest"
	"github.com/ethereum-optimism/optimism/op-devstack/presets"
	"github.com/ethereum-optimism/optimism/op-devstack/sysgo"
	"github.com/ethereum-optimism/optimism/op-service/eth"

	xcommon "github.com/okx/xlayer-reth/tests/common"
)

// xlayerBinaryOpts loads the harness config and returns the preset options that
// point every XLayer devnet node at the prebuilt RUST_BINARY_PATH_OP_RETH execution
// client. The XLayer reth binary carries the built-in flashblocks builder, so no
// op-rbuilder/rollup-boost process is needed.
func xlayerBinaryOpts(t devtest.T) []presets.Option {
	cfg, err := xcommon.LoadXLayerConfig()
	t.Require().NoError(err, "harness config must load")
	t.Require().NoError(cfg.RequireExecutionBinary(), "RUST_BINARY_PATH_OP_RETH must point to a built XLayer reth binary")
	return []presets.Option{
		presets.WithLocalContractSourcesAt(cfg.ForgeArtifactsDir()),
	}
}

// callRPC issues a raw JSON-RPC call against the sequencer EL and fails the test
// if the node returns a transport/handler error. Individual scenarios decode the
// result into whatever shape they assert on.
func callRPC(t devtest.T, sys *presets.XLayer, out any, method string, args ...any) {
	err := sys.L2EL.EthClient().RPC().CallContext(t.Ctx(), out, method, args...)
	t.Require().NoErrorf(err, "rpc %s must not return a process-level error", method)
}

func callUintConfig(t devtest.T, sys *presets.XLayer, systemConfig common.Address, signature, returns string, out any) {
	fn := w3.MustNewFunc(signature, returns)
	data, err := fn.EncodeArgs()
	t.Require().NoError(err, "encode %s", signature)
	result, err := sys.L1EL.EthClient().Call(t.Ctx(), ethereum.CallMsg{To: &systemConfig, Data: data}, gethrpc.LatestBlockNumber)
	t.Require().NoError(err, "call SystemConfig.%s", signature)
	t.Require().NoError(fn.DecodeReturns(result, out), "decode SystemConfig.%s", signature)
}

// TestRPC exercises the XLayer JSON-RPC surface. All cases share a single devnet
// because the topology is identical across them, then run in parallel with
// independent funded senders and recipients for cases that submit transactions.
func TestRPC(gt *testing.T) {
	t := devtest.ParallelT(gt)
	legacy, legacyURL := startLegacySentinel(t)
	opts := append(xlayerBinaryOpts(t), presets.WithOpRethOption(sysgo.OpRethWithExtraArgs(
		"--rpc.legacy-url="+legacyURL,
		"--rpc.legacy-timeout=5s",
	)))
	sys := presets.NewXLayer(t, opts...)

	gt.Run("LegacyRouting", func(gt *testing.T) {
		t := devtest.ParallelT(gt)
		assertLegacyRPCRouting(t, sys, legacy)
	})

	// TestSendTransaction: a native transfer is reflected in the recipient balance
	// at the inclusion block.
	gt.Run("SendTransaction", func(gt *testing.T) {
		t := devtest.ParallelT(gt)
		alice := sys.FunderL2.NewFundedEOA(eth.OneEther)
		bob := sys.Wallet.NewEOA(sys.L2EL)
		bobAddr := bob.Address()

		receipt := xcommon.SendTransfer(t, alice, bobAddr, eth.OneHundredthEther)

		bal, err := sys.L2EL.EthClient().BalanceAt(t.Ctx(), bobAddr, receipt.BlockNumber)
		t.Require().NoError(err)
		t.Require().Equal(0, bal.Cmp(eth.OneHundredthEther.ToBig()), "recipient balance must equal the transferred value")
	})

	// EthereumBasicRPC: the read-only Ethereum JSON-RPC surface (chainId, syncing,
	// balance, code, block number, nonce, gas price, storage).
	gt.Run("EthereumBasicRPC", func(gt *testing.T) {
		t := devtest.ParallelT(gt)
		alice := sys.FunderL2.NewFundedEOA(eth.OneEther)
		addr := alice.Address()

		var chainID hexutil.Big
		callRPC(t, sys, &chainID, "eth_chainId")
		t.Require().Equal(
			0,
			(*big.Int)(&chainID).Cmp(new(big.Int).SetUint64(sysgo.XLayerDefaultL2ChainID)),
			"chain id must match the XLayer toolkit devnet",
		)

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
	})

	// FeeMarketParity verifies the live L1 SystemConfig and L2 genesis values,
	// rather than merely checking deployer intent. This catches parameters that
	// could otherwise be overwritten by the first L1 attributes transaction.
	gt.Run("FeeMarketParity", func(gt *testing.T) {
		t := devtest.ParallelT(gt)
		systemConfig := xcommon.SystemConfigAddress(t, sys)

		var denominator, elasticity, baseFeeScalar, blobBaseFeeScalar uint32
		var minBaseFee uint64
		var daFootprintGasScalar uint16
		callUintConfig(t, sys, systemConfig, "eip1559Denominator()", "uint32", &denominator)
		callUintConfig(t, sys, systemConfig, "eip1559Elasticity()", "uint32", &elasticity)
		callUintConfig(t, sys, systemConfig, "basefeeScalar()", "uint32", &baseFeeScalar)
		callUintConfig(t, sys, systemConfig, "blobbasefeeScalar()", "uint32", &blobBaseFeeScalar)
		callUintConfig(t, sys, systemConfig, "minBaseFee()", "uint64", &minBaseFee)
		callUintConfig(t, sys, systemConfig, "daFootprintGasScalar()", "uint16", &daFootprintGasScalar)

		t.Require().Equal(uint32(sysgo.XLayerEIP1559Denominator), denominator)
		t.Require().Equal(uint32(sysgo.XLayerEIP1559Elasticity), elasticity)
		t.Require().Equal(sysgo.XLayerEcotoneBaseFeeScalar, baseFeeScalar)
		t.Require().Equal(sysgo.XLayerEcotoneBlobBaseFeeScalar, blobBaseFeeScalar)
		t.Require().Equal(sysgo.XLayerMinBaseFee, minBaseFee)
		t.Require().Equal(uint16(0), daFootprintGasScalar, "XLayer toolkit intentionally disables DA-footprint gas accounting")

		var genesis struct {
			BaseFeePerGas *hexutil.Big   `json:"baseFeePerGas"`
			GasLimit      hexutil.Uint64 `json:"gasLimit"`
		}
		callRPC(t, sys, &genesis, "eth_getBlockByNumber", hexutil.EncodeUint64(sysgo.XLayerDefaultL2GenesisHeight), false)
		t.Require().NotNil(genesis.BaseFeePerGas)
		t.Require().Equal(0, (*big.Int)(genesis.BaseFeePerGas).Cmp(new(big.Int).SetUint64(sysgo.XLayerGenesisBaseFeePerGas)))
		t.Require().Equal(sysgo.XLayerGenesisGasLimit, uint64(genesis.GasLimit))
	})

	// EthBlockRPC: block-oriented queries by hash and number, plus block
	// transaction counts, using a block that includes a real transfer.
	gt.Run("EthBlockRPC", func(gt *testing.T) {
		t := devtest.ParallelT(gt)
		alice := sys.FunderL2.NewFundedEOA(eth.OneEther)
		bob := sys.Wallet.NewEOA(sys.L2EL)
		bobAddr := bob.Address()
		receipt := xcommon.SendTransfer(t, alice, bobAddr, eth.OneHundredthEther)
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
	})

	// EthTransactionRPC: transaction-oriented queries and gas estimation against a
	// canonical transaction.
	gt.Run("EthTransactionRPC", func(gt *testing.T) {
		t := devtest.ParallelT(gt)
		alice := sys.FunderL2.NewFundedEOA(eth.OneEther)
		bob := sys.Wallet.NewEOA(sys.L2EL)
		bobAddr := bob.Address()
		receipt := xcommon.SendTransfer(t, alice, bobAddr, eth.OneHundredthEther)

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
	})

	// DebugTraceRPC: the debug trace surface returns success semantics for a
	// canonical transaction and its block.
	gt.Run("DebugTraceRPC", func(gt *testing.T) {
		t := devtest.ParallelT(gt)
		alice := sys.FunderL2.NewFundedEOA(eth.OneEther)
		bob := sys.Wallet.NewEOA(sys.L2EL)
		bobAddr := bob.Address()
		receipt := xcommon.SendTransfer(t, alice, bobAddr, eth.OneHundredthEther)
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
	})

	// EthLogsRPC: eth_getLogs by range and by block hash, asserting the node
	// returns a well-formed (possibly empty) result rather than an error.
	gt.Run("EthLogsRPC", func(gt *testing.T) {
		t := devtest.ParallelT(gt)
		alice := sys.FunderL2.NewFundedEOA(eth.OneEther)
		bob := sys.Wallet.NewEOA(sys.L2EL)
		bobAddr := bob.Address()
		receipt := xcommon.SendTransfer(t, alice, bobAddr, eth.OneHundredthEther)

		var byRange []json.RawMessage
		callRPC(t, sys, &byRange, "eth_getLogs", map[string]any{"fromBlock": "earliest", "toBlock": "latest"})

		var byBlockHash []json.RawMessage
		callRPC(t, sys, &byBlockHash, "eth_getLogs", map[string]any{"blockHash": receipt.BlockHash.Hex()})
	})

	// TxpoolRPC: the txpool inspection surface.
	gt.Run("TxpoolRPC", func(gt *testing.T) {
		t := devtest.ParallelT(gt)
		var content json.RawMessage
		callRPC(t, sys, &content, "txpool_content")
		t.Require().NotEmpty(content, "txpool_content must return a result")

		var status map[string]hexutil.Uint64
		callRPC(t, sys, &status, "txpool_status")
		_, hasPending := status["pending"]
		t.Require().True(hasPending, "txpool_status must report a pending count")
	})

	// FeeHistory verifies that the EIP-1559 fee-history RPC returns a result for
	// recent canonical blocks.
	gt.Run("FeeHistory", func(gt *testing.T) {
		t := devtest.ParallelT(gt)
		var feeHistory json.RawMessage
		callRPC(t, sys, &feeHistory, "eth_feeHistory", "0x4", "latest", []float64{25, 75})
		t.Require().NotEmpty(feeHistory, "eth_feeHistory must return a result")
	})
}
