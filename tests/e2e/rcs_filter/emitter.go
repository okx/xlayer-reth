package rcs_filter

import (
	"context"
	"time"

	"github.com/ethereum/go-ethereum/common"
	"github.com/ethereum/go-ethereum/core/types"

	"github.com/ethereum-optimism/optimism/op-devstack/devtest"
	"github.com/ethereum-optimism/optimism/op-devstack/dsl"
	"github.com/ethereum-optimism/optimism/op-devstack/presets"
	"github.com/ethereum-optimism/optimism/op-service/eth"
	"github.com/ethereum-optimism/optimism/op-service/txplan"
)

// transferTopic0 is keccak256("Transfer(address,address,uint256)") — the canonical ERC-20 Transfer
// event signature hash. The emergency rule fixture (testdata/emergency_rules.json) declares this
// exact event, so a log the emitter produces with this topic0 genuinely matches the rule's declared
// event at screen time (topic0 is a physical log topic, NOT calldata). emitter_test.go asserts this
// constant equals the live keccak of the signature so it can never silently drift.
var transferTopic0 = common.HexToHash("0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef")

// emitterCreationBytecode is a minimal, hand-assembled contract (Decision D6): self-contained EVM
// creation bytecode, no Solidity/forge, no deps/optimism touch. On ANY call its deployed runtime:
//  1. MSTORE(0, 0x2a)                  // stage a 32-byte value word for the log data
//  2. SSTORE(slot 0, 1)               // deterministic, owned "counter" slot → observable state write
//  3. LOG3(topic0=Transfer, from, to) // a real ERC-20-shaped Transfer event; data = mem[0..32]
//  4. STOP                            // succeed (no REVERT) so the log is present in result.logs()
//
// Because the emergency rule denies on the tx-level contract_address BEFORE the builder commits
// state (crates/builder/src/flashblocks/context.rs: screen against result.logs() then Screen::Deny
// -> continue, all before evm.db_mut().commit(state)), a denied call to this contract emits its
// Transfer at screen time yet its slot-0 write is rolled back — the property the event-deny scenario
// asserts.
//
// Byte layout (65 bytes total):
//
//	constructor (12 bytes): 6035 600c 6000 39 6035 6000 f3
//	  PUSH1 0x35(len=53); PUSH1 0x0c(runtime offset); PUSH1 0x00(dest); CODECOPY;
//	  PUSH1 0x35(len); PUSH1 0x00(offset); RETURN   -> returns the 53-byte runtime
//	runtime (53 bytes): 602a 6000 52  6001 6000 55  6002 6001  7f<32-byte Transfer topic0>  6020 6000 a3  00
//	  PUSH1 0x2a; PUSH1 0x00; MSTORE;  PUSH1 0x01; PUSH1 0x00; SSTORE;
//	  PUSH1 0x02(topic3=to); PUSH1 0x01(topic2=from); PUSH32 topic0(Transfer);
//	  PUSH1 0x20(size); PUSH1 0x00(offset); LOG3;  STOP
var emitterCreationBytecode = common.FromHex(
	"0x6035600c60003960356000f3" +
		"602a6000526001600055600260017f" +
		"ddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef" +
		"60206000a300",
)

// emitterDeployGasLimit comfortably covers constructor execution plus the 53-byte runtime code
// deposit; the emitter is deployed while rules are empty, so the creation tx is allowed.
const emitterDeployGasLimit uint64 = 500_000

// emitterCalldata returns calldata for an emit call. The runtime ignores calldata shape (any call
// emits + stores), so an empty payload suffices.
func emitterCalldata() []byte { return nil }

// deployEmitter deploys emitterCreationBytecode via a contract-creation transaction and returns the
// deployed address. It MUST be called while the mock still serves empty rules so the creation tx is
// allowed and committed (persisting the runtime code and a zero slot-0 baseline).
func deployEmitter(t devtest.T, sys *presets.XLayer, owner *dsl.EOA) common.Address {
	ptx := txplan.NewPlannedTx(
		owner.Plan(),
		txplan.WithValue(eth.ZeroWei),
		txplan.WithData(emitterCreationBytecode),
		txplan.WithGasLimit(emitterDeployGasLimit),
	)
	ctx, cancel := context.WithTimeout(t.Ctx(), 30*time.Second)
	defer cancel()
	receipt, err := ptx.Included.Eval(ctx)
	t.Require().NoError(err, "emitter deployment must be included while rules are empty")
	t.Require().Equal(types.ReceiptStatusSuccessful, receipt.Status, "emitter deployment must succeed")
	t.Require().NotEqual(common.Address{}, receipt.ContractAddress, "deployment receipt must carry a contract address")
	return receipt.ContractAddress
}

// readCounterSlot reads storage slot 0 (the emitter's counter) via eth_getStorageAt. A denied call
// must leave this at its pre-call baseline because deny short-circuits before state commit.
func readCounterSlot(t devtest.T, sys *presets.XLayer, addr common.Address) common.Hash {
	var slot common.Hash
	err := sys.L2EL.EthClient().RPC().CallContext(t.Ctx(), &slot, "eth_getStorageAt", addr, "0x0", "latest")
	t.Require().NoError(err, "read emitter storage slot 0")
	return slot
}
