package rcs_filter

import (
	"context"
	"testing"
	"time"

	"github.com/ethereum/go-ethereum/common"
	"github.com/ethereum/go-ethereum/common/hexutil"
	"github.com/ethereum/go-ethereum/core/types"

	"github.com/ethereum-optimism/optimism/op-devstack/devtest"
	"github.com/ethereum-optimism/optimism/op-devstack/dsl"
	"github.com/ethereum-optimism/optimism/op-devstack/presets"
	"github.com/ethereum-optimism/optimism/op-devstack/sysgo"
	"github.com/ethereum-optimism/optimism/op-service/eth"
	"github.com/ethereum-optimism/optimism/op-service/txplan"

	xcommon "github.com/okx/xlayer-reth/tests/common"
)

// txBlacklist is the chain-195 TxBlacklist address the emergency rule excepts (lowercase).
var txBlacklist = common.HexToAddress("0xb1ac000000000000000000000000000000000001")

// denyReceiptTimeout bounds how long we wait before concluding a transaction was denied
// (never included). It must exceed several block-production cycles.
const denyReceiptTimeout = 12 * time.Second

// probeGasLimit is a fixed gas cap for the call/create probes so txplan does not have to
// gas-estimate a transaction the emergency rule is expected to deny (estimation of a denied
// tx is unnecessary and its result is unused).
const probeGasLimit uint64 = 200_000

func newXLayerRCS(gt *testing.T) (devtest.T, *presets.XLayer, *MockRCS) {
	t := devtest.ParallelT(gt)
	cfg, err := xcommon.LoadXLayerConfig()
	t.Require().NoError(err, "harness config must load")
	t.Require().NoError(cfg.RequireExecutionBinary(),
		"RUST_BINARY_PATH_OP_RETH must point to a system-installed XLayer reth binary")

	mock := StartMockRCS(t)
	opts := []presets.Option{
		presets.WithLocalContractSourcesAt(cfg.ForgeArtifactsDir()),
		// Enable the RCS filter and point it at the mock. Uses the same node-flag path as the
		// gasless/rpc suites, so deps/optimism is untouched. `--rcs-filter.enabled` is a bool
		// arg with an explicit default, so it must carry a value (`=true`).
		presets.WithOpRethOption(sysgo.OpRethWithExtraArgs(
			"--rcs-filter.enabled=true",
			"--rcs-filter.rcs-base-url="+mock.URL(),
		)),
	}
	return t, presets.NewXLayer(t, opts...), mock
}

// headBlock reads the current L2 head block number from the sequencer EL.
func headBlock(t devtest.T, sys *presets.XLayer) uint64 {
	var n hexutil.Uint64
	err := sys.L2EL.EthClient().RPC().CallContext(t.Ctx(), &n, "eth_blockNumber")
	t.Require().NoError(err, "read L2 head block number")
	return uint64(n)
}

// included submits a planned transaction and reports whether it reached a successful canonical
// receipt within timeout. A transaction the RCS filter denies is rejected at txpool ingress (the
// submit fails) or is never built into a block (the receipt poll times out); both map to false, so
// a false result means "denied / not included" and true means "allowed and mined". It never fails
// the test — the caller asserts the boolean — which is why the failing-on-timeout tests/common
// helpers (SendTransfer / WaitForReceipt) are not reused here.
func included(t devtest.T, sys *presets.XLayer, ptx *txplan.PlannedTx, timeout time.Duration) bool {
	ctx, cancel := context.WithTimeout(t.Ctx(), timeout)
	defer cancel()

	if _, err := ptx.Submitted.Eval(ctx); err != nil {
		// Rejected before entering the pool (the deny path for txpool-ingress screening).
		return false
	}
	signed, err := ptx.Signed.Eval(ctx)
	if err != nil {
		return false
	}
	hash := signed.Hash()

	client := sys.L2EL.EthClient()
	ticker := time.NewTicker(300 * time.Millisecond)
	defer ticker.Stop()
	for {
		receipt, err := client.TransactionReceipt(ctx, hash)
		if err == nil && receipt != nil {
			return receipt.Status == types.ReceiptStatusSuccessful
		}
		select {
		case <-ctx.Done():
			return false
		case <-ticker.C:
		}
	}
}

// transferIncluded submits a native-value transfer and returns whether it was included.
func transferIncluded(t devtest.T, sys *presets.XLayer, sender *dsl.EOA, to common.Address, timeout time.Duration) bool {
	ptx := txplan.NewPlannedTx(sender.Plan(), txplan.WithTo(&to), txplan.WithValue(eth.OneGWei))
	return included(t, sys, ptx, timeout)
}

// callIncluded submits a zero-value contract call carrying data and returns whether it was
// included. Passing nil data yields a plain no-log call.
func callIncluded(t devtest.T, sys *presets.XLayer, sender *dsl.EOA, to common.Address, data []byte, timeout time.Duration) bool {
	ptx := txplan.NewPlannedTx(
		sender.Plan(),
		txplan.WithTo(&to),
		txplan.WithValue(eth.ZeroWei),
		txplan.WithData(data),
		txplan.WithGasLimit(probeGasLimit),
	)
	return included(t, sys, ptx, timeout)
}

// createIncluded submits a contract-creation transaction (no `to`) and returns whether it was
// included. tx.to == nil binds contract_address to null in the matcher.
func createIncluded(t devtest.T, sys *presets.XLayer, sender *dsl.EOA, initCode []byte, timeout time.Duration) bool {
	ptx := txplan.NewPlannedTx(
		sender.Plan(),
		txplan.WithValue(eth.ZeroWei),
		txplan.WithData(initCode),
		txplan.WithGasLimit(probeGasLimit),
	)
	return included(t, sys, ptx, timeout)
}

// contractInitBytecode returns minimal init code that deploys an empty contract and succeeds:
// PUSH1 0x00 PUSH1 0x00 RETURN.
func contractInitBytecode() []byte {
	return []byte{0x60, 0x00, 0x60, 0x00, 0xf3}
}

// activateAndConfirmInstalled flips the mock to the emergency rule set and confirms the node has
// actually installed it: a transfer to a normal target that was includable under the empty rule
// set must stop being included. Polling guards against mistaking transient fetch/parse failures
// for a successful install.
func activateAndConfirmInstalled(t devtest.T, sys *presets.XLayer, mock *MockRCS, probeSender *dsl.EOA) {
	// Baseline: with empty rules, a normal transfer is included.
	t.Require().True(
		transferIncluded(t, sys, probeSender,
			common.HexToAddress("0x00000000000000000000000000000000000d1234"), denyReceiptTimeout),
		"a normal transfer must be included while the rule set is empty",
	)

	mock.ActivateEmergencyRules()
	t.Require().Equal(uint64(2), mock.ContentVersion(), "mock must serve emergency content_version 2")

	// Poll until a fresh transfer is no longer includable, i.e. the deny-all rule is live.
	t.Require().Eventually(func() bool {
		to := common.HexToAddress("0x00000000000000000000000000000000000dbeef")
		return !transferIncluded(t, sys, probeSender, to, denyReceiptTimeout)
	}, 60*time.Second, time.Second, "emergency rule must become active and start denying")
}

// TestEmergencyDenyAll drives the emergency deny-all rule end to end against a system-installed
// node binary and the in-repo mock RCS: every no-log transaction class is denied, the TxBlacklist
// exception is included, blocks keep advancing, no audit traffic is emitted in the deny window,
// and clearing the rules restores normal transfers.
func TestEmergencyDenyAll(gt *testing.T) {
	t, sys, mock := newXLayerRCS(gt)

	// Distinct senders per transaction class so a per-sender nonce stall cannot masquerade as a
	// deny (a denied tx never consumes its nonce, but isolating senders removes all ambiguity).
	native := sys.FunderL2.NewFundedEOA(eth.OneEther)
	nolog := sys.FunderL2.NewFundedEOA(eth.OneEther)
	event := sys.FunderL2.NewFundedEOA(eth.OneEther)
	create := sys.FunderL2.NewFundedEOA(eth.OneEther)
	exception := sys.FunderL2.NewFundedEOA(eth.OneEther)

	// D6: deploy the emitter WHILE RULES ARE EMPTY so its creation tx is allowed and committed
	// (its runtime code + a zero slot-0 baseline persist). Must precede activation — a creation
	// submitted after activation would itself be denied by the contract-creation class. `event`
	// owns it and later makes the (denied) emit call.
	emitter := deployEmitter(t, sys, event)
	baseSlot0 := readCounterSlot(t, sys, emitter)
	t.Require().Equal(common.Hash{}, baseSlot0, "emitter counter (slot 0) must start at zero")

	// Install the emergency rules and confirm they are actually live.
	activateAndConfirmInstalled(t, sys, mock, native)

	blockBefore := headBlock(t, sys)
	submitBefore, queryBefore := mock.SubmitCount(), mock.QueryCount()

	// (a) native transfer to a normal target → denied (never included).
	t.Require().False(transferIncluded(t, sys, native,
		common.HexToAddress("0x00000000000000000000000000000000000d0001"), denyReceiptTimeout),
		"native transfer to a normal target must be denied")

	// (b) no-log contract call to a normal target → denied.
	t.Require().False(callIncluded(t, sys, nolog,
		common.HexToAddress("0x00000000000000000000000000000000000d0002"), nil, denyReceiptTimeout),
		"no-log call to a normal target must be denied")

	// (c) event-emitting call → denied, EVEN THOUGH it really emits the rule-declared Transfer
	// event (topic0 matches). Calls the pre-deployed emitter (a real event, not calldata that
	// merely looks like one against a code-less address). Deny short-circuits BEFORE state commit,
	// so the emitter's counter slot stays at baseline. This is the AC#9 event-class coverage the
	// first round lacked (MR !72 review).
	t.Require().False(callIncluded(t, sys, event, emitter, emitterCalldata(), denyReceiptTimeout),
		"event-emitting call to the emitter must be denied even though it emits a matching Transfer")
	t.Require().Equal(baseSlot0, readCounterSlot(t, sys, emitter),
		"denied emit-call must not commit: emitter counter (slot 0) must be unchanged")

	// (d) contract creation (to == nil) → denied.
	t.Require().False(createIncluded(t, sys, create, contractInitBytecode(), denyReceiptTimeout),
		"contract creation must be denied")

	// (e) transfer to the TxBlacklist exception target → included (gets a receipt).
	t.Require().True(transferIncluded(t, sys, exception, txBlacklist, denyReceiptTimeout),
		"transfer to the TxBlacklist contract must be allowed and included")

	// Chain keeps producing blocks during the emergency (the exception tx alone advanced it, but
	// assert progress explicitly).
	t.Require().Eventually(func() bool {
		return headBlock(t, sys) > blockBefore
	}, 20*time.Second, time.Second, "blocks must keep being produced during the emergency")

	// Zero audit traffic in the deny-only window: denies and the exception carry no audit content.
	t.Require().Equal(submitBefore, mock.SubmitCount(), "no RCS submit traffic during deny-only window")
	t.Require().Equal(queryBefore, mock.QueryCount(), "no RCS query traffic during deny-only window")

	// Recovery: revert to empty rules; normal transfers resume.
	mock.RestoreEmptyRules()
	t.Require().Eventually(func() bool {
		to := common.HexToAddress("0x00000000000000000000000000000000000d9999")
		return transferIncluded(t, sys, native, to, denyReceiptTimeout)
	}, 60*time.Second, time.Second, "normal transfers must resume after rules are cleared")
}
