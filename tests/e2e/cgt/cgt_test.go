package cgt

import (
	"math/big"
	"path/filepath"
	"testing"
	"time"

	"github.com/ethereum/go-ethereum"
	"github.com/ethereum/go-ethereum/common"
	"github.com/ethereum/go-ethereum/core/types"
	"github.com/ethereum/go-ethereum/crypto"
	"github.com/ethereum/go-ethereum/rpc"
	"github.com/lmittmann/w3"

	"github.com/ethereum-optimism/optimism/op-chain-ops/devkeys"
	"github.com/ethereum-optimism/optimism/op-chain-ops/foundry"
	"github.com/ethereum-optimism/optimism/op-core/predeploys"
	"github.com/ethereum-optimism/optimism/op-devstack/devtest"
	"github.com/ethereum-optimism/optimism/op-devstack/dsl"
	"github.com/ethereum-optimism/optimism/op-devstack/presets"
	"github.com/ethereum-optimism/optimism/op-devstack/sysgo"
	"github.com/ethereum-optimism/optimism/op-node/rollup/derive"
	"github.com/ethereum-optimism/optimism/op-service/apis"
	"github.com/ethereum-optimism/optimism/op-service/eth"
	"github.com/ethereum-optimism/optimism/op-service/txplan"

	xcommon "github.com/okx/xlayer-reth/tests/common"
)

const (
	cgtName           = "OKB"
	cgtSymbol         = "OKB"
	cgtL1TokenName    = "Mock OKB"
	cgtReceiptTimeout = 20 * time.Second
	cgtSetupGasLimit  = 5_000_000
)

func xlayerCGTOpts(t devtest.T, cfg *xcommon.XLayerConfig) []presets.Option {
	return []presets.Option{
		presets.WithLocalContractSourcesAt(cfg.ForgeArtifactsDir()),
		presets.WithDeployerOptions(
			sysgo.WithCustomGasToken(cgtName, cgtSymbol, big.NewInt(0), common.Address{}),
		),
	}
}

type cgtDepositFixture struct {
	token          common.Address
	adapter        common.Address
	owner          common.Address
	portal         common.Address
	recipient      common.Address
	amount         *big.Int
	l2SeqReceipt   *types.Receipt
	l2ValidReceipt *types.Receipt
}

func artifactDeployData(t devtest.T, artifact *foundry.Artifact, args ...any) []byte {
	constructorArgs, err := artifact.ABI.Pack("", args...)
	t.Require().NoError(err, "encode constructor arguments")
	return append(append([]byte(nil), artifact.Bytecode.Object...), constructorArgs...)
}

func encodeCall(t devtest.T, signature string, args ...any) []byte {
	fn := w3.MustNewFunc(signature, "")
	data, err := fn.EncodeArgs(args...)
	t.Require().NoError(err, "encode %s", signature)
	return data
}

// setupCGTDeposit mirrors xlayer-toolkit's DeployMockOKB and
// SetupCustomGasToken scripts without spawning forge or exposing a dev key.
// Consecutive nonces let all dependent L1 transactions execute in one block;
// fixed gas limits avoid estimating calls against contracts not canonical yet.
// UPSTREAM(optimism): this setup can become a reusable sysgo CGT fixture once
// WithCustomGasToken supports deploying/configuring the L1 token and adapter.
func setupCGTDeposit(t devtest.T, sys *presets.XLayer, cfg *xcommon.XLayerConfig) cgtDepositFixture {
	keys := sys.L2Chain.Escape().Keys()
	ownerKey := devkeys.SystemConfigOwner.Key(sys.L2Chain.ChainID().ToBig())
	owner := dsl.NewKey(t, keys.Secret(ownerKey)).User(sys.L1EL)
	baseNonce := owner.PendingNonce()
	token := crypto.CreateAddress(owner.Address(), baseNonce)
	adapter := crypto.CreateAddress(owner.Address(), baseNonce+1)
	portal := sys.L2Chain.DepositContractAddr()
	systemConfig := xcommon.SystemConfigAddress(t, sys)
	recipient := sys.Wallet.NewEOA(sys.L2EL).Address()
	amount := big.NewInt(7_999_000_000_000_000)

	mockArtifact, err := foundry.ReadArtifact(filepath.Join(cfg.ForgeArtifactsDir(), "DeployMockOKB.s.sol", "MockOKB.json"))
	t.Require().NoError(err, "read MockOKB artifact")
	adapterArtifact, err := foundry.ReadArtifact(filepath.Join(cfg.ForgeArtifactsDir(), "DepositedOKBAdapter.sol", "DepositedOKBAdapter.json"))
	t.Require().NoError(err, "read DepositedOKBAdapter artifact")

	var name, symbol [32]byte
	copy(name[:], cgtL1TokenName)
	copy(symbol[:], cgtSymbol)
	calls := []struct {
		to   *common.Address
		data []byte
	}{
		{data: artifactDeployData(t, mockArtifact)},
		{data: artifactDeployData(t, adapterArtifact, token, portal, owner.Address())},
		{to: &systemConfig, data: encodeCall(t, "setGasPayingToken(address,uint8,bytes32,bytes32)", adapter, uint8(18), name, symbol)},
		{to: &adapter, data: encodeCall(t, "addToWhitelistBatch(address[])", []common.Address{owner.Address()})},
		{to: &token, data: encodeCall(t, "approve(address,uint256)", adapter, amount)},
		{to: &adapter, data: encodeCall(t, "deposit(address,uint256)", recipient, amount)},
	}

	plans := make([]*txplan.PlannedTx, len(calls))
	for i, call := range calls {
		opts := []txplan.Option{
			owner.Plan(),
			txplan.WithStaticNonce(baseNonce + uint64(i)),
			txplan.WithData(call.data),
			txplan.WithGasLimit(cgtSetupGasLimit),
		}
		if call.to != nil {
			opts = append(opts, txplan.WithTo(call.to))
		}
		plans[i] = txplan.NewPlannedTx(opts...)
		_, err := plans[i].Submitted.Eval(t.Ctx())
		t.Require().NoErrorf(err, "submit CGT setup transaction %d", i)
	}

	_, err = plans[len(plans)-1].Success.Eval(t.Ctx())
	t.Require().NoError(err, "final CGT setup transaction must succeed")
	receipts := make([]*types.Receipt, len(plans))
	for i, plan := range plans {
		receipts[i], err = plan.Included.Eval(t.Ctx())
		t.Require().NoErrorf(err, "CGT setup transaction %d must be included", i)
		t.Require().Equal(types.ReceiptStatusSuccessful, receipts[i].Status, "CGT setup transaction %d must succeed", i)
	}
	t.Require().Equal(token, receipts[0].ContractAddress, "MockOKB deployment address")
	t.Require().Equal(adapter, receipts[1].ContractAddress, "DepositedOKBAdapter deployment address")

	var depositTx *types.DepositTx
	for _, log := range receipts[len(receipts)-1].Logs {
		if parsed, parseErr := derive.UnmarshalDepositLogEvent(log); parseErr == nil {
			depositTx = parsed
			break
		}
	}
	t.Require().NotNil(depositTx, "adapter deposit must emit TransactionDeposited")
	depositHash := types.NewTx(depositTx).Hash()

	return cgtDepositFixture{
		token:          token,
		adapter:        adapter,
		owner:          owner.Address(),
		portal:         portal,
		recipient:      recipient,
		amount:         amount,
		l2SeqReceipt:   xcommon.WaitForReceipt(t, sys.L2EL.EthClient(), depositHash, cgtReceiptTimeout),
		l2ValidReceipt: xcommon.WaitForReceipt(t, sys.L2ELRPC1.EthClient(), depositHash, cgtReceiptTimeout),
	}
}

func callContract(t devtest.T, client apis.EthClient, to common.Address, fn *w3.Func) []byte {
	data, err := fn.EncodeArgs()
	t.Require().NoError(err, "encode %s", fn.Signature)
	out, err := client.Call(t.Ctx(), ethereum.CallMsg{To: &to, Data: data}, rpc.LatestBlockNumber)
	t.Require().NoError(err, "call %s", fn.Signature)
	return out
}

// TestCGT exercises the complete toolkit-style CGT path. One shared XLayer
// topology deploys MockOKB and DepositedOKBAdapter on L1, configures
// SystemConfig, and performs an ERC-20-to-L2-native deposit before the parallel
// assertions run.
func TestCGT(gt *testing.T) {
	t := devtest.ParallelT(gt)
	cfg, err := xcommon.LoadXLayerConfig()
	t.Require().NoError(err, "harness config must load")
	t.Require().NoError(cfg.RequireExecutionBinary(), "RUST_BINARY_PATH_OP_RETH must point to a built XLayer reth binary")
	sys := presets.NewXLayer(t, xlayerCGTOpts(t, cfg)...)
	deposit := setupCGTDeposit(t, sys, cfg)

	gt.Run("L1SystemConfigFlag", func(gt *testing.T) {
		t := devtest.ParallelT(gt)
		systemConfig := xcommon.SystemConfigAddress(t, sys)
		fn := w3.MustNewFunc("isCustomGasToken()", "bool")
		out := callContract(t, sys.L1EL.EthClient(), systemConfig, fn)
		var enabled bool
		t.Require().NoError(fn.DecodeReturns(out, &enabled), "decode SystemConfig.isCustomGasToken()")
		t.Require().True(enabled, "L1 SystemConfig must enable CGT")
	})

	gt.Run("L1AdapterConfiguration", func(gt *testing.T) {
		t := devtest.ParallelT(gt)
		systemConfig := xcommon.SystemConfigAddress(t, sys)

		gasTokenFn := w3.MustNewFunc("gasPayingToken()", "address,uint8")
		out := callContract(t, sys.L1EL.EthClient(), systemConfig, gasTokenFn)
		var gasToken common.Address
		var decimals uint8
		t.Require().NoError(gasTokenFn.DecodeReturns(out, &gasToken, &decimals))
		t.Require().Equal(deposit.adapter, gasToken, "SystemConfig must reference DepositedOKBAdapter")
		t.Require().Equal(uint8(18), decimals)

		nameFn := w3.MustNewFunc("gasPayingTokenName()", "string")
		out = callContract(t, sys.L1EL.EthClient(), systemConfig, nameFn)
		var name string
		t.Require().NoError(nameFn.DecodeReturns(out, &name))
		t.Require().Equal(cgtL1TokenName, name, "SystemConfig must expose the L1 token name")

		okbFn := w3.MustNewFunc("OKB()", "address")
		out = callContract(t, sys.L1EL.EthClient(), deposit.adapter, okbFn)
		var okb common.Address
		t.Require().NoError(okbFn.DecodeReturns(out, &okb))
		t.Require().Equal(deposit.token, okb)

		portalFn := w3.MustNewFunc("PORTAL()", "address")
		out = callContract(t, sys.L1EL.EthClient(), deposit.adapter, portalFn)
		var portal common.Address
		t.Require().NoError(portalFn.DecodeReturns(out, &portal))
		t.Require().Equal(deposit.portal, portal)

		ownerFn := w3.MustNewFunc("owner()", "address")
		out = callContract(t, sys.L1EL.EthClient(), deposit.adapter, ownerFn)
		var owner common.Address
		t.Require().NoError(ownerFn.DecodeReturns(out, &owner))
		t.Require().Equal(deposit.owner, owner)
	})

	gt.Run("L2Metadata", func(gt *testing.T) {
		t := devtest.ParallelT(gt)
		client := sys.L2EL.EthClient()
		// L1 SystemConfig describes the deployed adapter token ("Mock OKB"),
		// while L1BlockCGT reads the L2 native asset metadata initialized in the
		// LiquidityController genesis config ("OKB"). The L1 setter does not
		// rewrite LiquidityController metadata.

		enabledFn := w3.MustNewFunc("isCustomGasToken()", "bool")
		out := callContract(t, client, predeploys.L1BlockAddr, enabledFn)
		var enabled bool
		t.Require().NoError(enabledFn.DecodeReturns(out, &enabled), "decode L1Block.isCustomGasToken()")
		t.Require().True(enabled, "L2 L1Block must enable CGT")

		nameFn := w3.MustNewFunc("gasPayingTokenName()", "string")
		out = callContract(t, client, predeploys.L1BlockAddr, nameFn)
		var name string
		t.Require().NoError(nameFn.DecodeReturns(out, &name), "decode L1Block.gasPayingTokenName()")
		t.Require().Equal(cgtName, name, "L2 native gas token name")

		symbolFn := w3.MustNewFunc("gasPayingTokenSymbol()", "string")
		out = callContract(t, client, predeploys.L1BlockAddr, symbolFn)
		var symbol string
		t.Require().NoError(symbolFn.DecodeReturns(out, &symbol), "decode L1Block.gasPayingTokenSymbol()")
		t.Require().Equal(cgtSymbol, symbol, "L2 gas-paying token symbol")
	})

	gt.Run("ERC20Deposit", func(gt *testing.T) {
		t := devtest.ParallelT(gt)
		balance, err := sys.L2EL.EthClient().BalanceAt(t.Ctx(), deposit.recipient, deposit.l2SeqReceipt.BlockNumber)
		t.Require().NoError(err, "read deposited L2 balance")
		t.Require().Equal(0, balance.Cmp(deposit.amount), "L1 OKB deposit must mint the same native amount on L2")
		t.Require().Equal(deposit.l2SeqReceipt.BlockNumber, deposit.l2ValidReceipt.BlockNumber)

		seqRoot, seqHash := xcommon.BlockRootAndHash(t, sys.L2EL.EthClient(), deposit.l2SeqReceipt.BlockNumber.Uint64())
		valRoot, valHash := xcommon.BlockRootAndHash(t, sys.L2ELRPC1.EthClient(), deposit.l2ValidReceipt.BlockNumber.Uint64())
		t.Require().Equal(seqRoot, valRoot, "sequencer and validator must agree on the CGT deposit state root")
		t.Require().Equal(seqHash, valHash, "sequencer and validator must agree on the CGT deposit block hash")
	})

	gt.Run("PortalRejectsETHDeposit", func(gt *testing.T) {
		t := devtest.ParallelT(gt)
		portal := sys.L2Chain.DepositContractAddr()
		_, err := sys.L1EL.EthClient().EstimateGas(t.Ctx(), ethereum.CallMsg{
			To:    &portal,
			Value: common.Big1,
		})
		t.Require().Error(err, "CGT portal must reject a direct ETH deposit")
	})

	gt.Run("NativeTransferAndValidatorAgreement", func(gt *testing.T) {
		t := devtest.ParallelT(gt)
		sequencer := sys.L2EL.EthClient()
		validator := sys.L2ELRPC1.EthClient()
		sender := sys.FunderL2.NewFundedEOA(eth.OneEther)
		recipient := sys.Wallet.NewEOA(sys.L2EL)
		amount := eth.OneHundredthEther
		senderBefore := sender.GetBalance()
		recipientBefore := recipient.GetBalance()

		receipt := xcommon.SendTransfer(t, sender, recipient.Address(), amount)
		recipientAfter, err := sequencer.BalanceAt(t.Ctx(), recipient.Address(), receipt.BlockNumber)
		t.Require().NoError(err, "read recipient balance at inclusion block")
		t.Require().Equal(0, recipientAfter.Cmp(recipientBefore.Add(amount).ToBig()), "recipient must receive CGT-native value")

		senderAfter, err := sequencer.BalanceAt(t.Ctx(), sender.Address(), receipt.BlockNumber)
		t.Require().NoError(err, "read sender balance at inclusion block")
		spent := new(big.Int).Sub(senderBefore.ToBig(), senderAfter)
		t.Require().Greater(spent.Cmp(amount.ToBig()), 0, "sender must pay transfer value plus gas in the native CGT balance")

		validatorReceipt := xcommon.WaitForReceipt(t, validator, receipt.TxHash, cgtReceiptTimeout)
		t.Require().Equal(receipt.BlockNumber, validatorReceipt.BlockNumber, "validator must import the CGT transaction at the same height")
		seqRoot, seqHash := xcommon.BlockRootAndHash(t, sequencer, receipt.BlockNumber.Uint64())
		valRoot, valHash := xcommon.BlockRootAndHash(t, validator, receipt.BlockNumber.Uint64())
		t.Require().Equal(seqRoot, valRoot, "sequencer and validator must agree on the CGT block state root")
		t.Require().Equal(seqHash, valHash, "sequencer and validator must agree on the CGT block hash")
	})
}
