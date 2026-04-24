package devstack

import (
	"encoding/hex"
	"fmt"
	"os"
	"os/signal"
	"path/filepath"
	"runtime"
	"syscall"
	"testing"

	"github.com/ethereum-optimism/optimism/op-devstack/devtest"
	"github.com/ethereum-optimism/optimism/op-devstack/dsl"
	"github.com/ethereum-optimism/optimism/op-devstack/presets"
	"github.com/ethereum-optimism/optimism/op-devstack/sysgo"
	"github.com/ethereum-optimism/optimism/op-service/eth"
	"github.com/ethereum-optimism/optimism/op-service/log/logfilter"
	"github.com/ethereum-optimism/optimism/op-service/logmods"
	gethlog "github.com/ethereum/go-ethereum/log"
	"github.com/ethereum/go-ethereum/crypto"
)

// op-devstack discovers packages/contracts-bedrock by walking up at most 6
// directories from cwd. Our test binary runs from scripts/aa/devstack/, so we
// chdir to deps/optimism/ where the walk immediately finds the artifacts.
func init() {
	_, thisFile, _, ok := runtime.Caller(0)
	if !ok {
		panic("runtime.Caller failed")
	}
	optimismDir := filepath.Join(filepath.Dir(thisFile), "..", "..", "..", "deps", "optimism")
	if err := os.Chdir(optimismDir); err != nil {
		panic(fmt.Sprintf("chdir to %s: %v", optimismDir, err))
	}
}

// TestXLayerAAHoldAlive sets up L1 + L2 (sequencer + validator) using op-devstack
// sysgo, then blocks until SIGINT so the operator can curl/cast against the
// running devnet.
//
// Defaults both L2 ELs to op-reth. RUST_BINARY_PATH_OP_RETH must point to the
// xlayer-reth-node binary.
func TestXLayerAAHoldAlive(gt *testing.T) {
	t := devtest.SerialT(gt)

	// Suppress INFO logs from the devstack framework so only the fmt.Printf banner
	// (and any warnings/errors) reaches the terminal.
	if h, ok := logmods.FindHandler[logfilter.FilterHandler](t.Logger().Handler()); ok {
		h.Set(logfilter.DefaultMute(logfilter.Level(gethlog.LevelError).Show()))
	}

	if os.Getenv("OP_DEVSTACK_PROOF_SEQUENCER_EL") == "" {
		os.Setenv("OP_DEVSTACK_PROOF_SEQUENCER_EL", "op-reth")
	}
	if os.Getenv("OP_DEVSTACK_PROOF_VALIDATOR_EL") == "" {
		os.Setenv("OP_DEVSTACK_PROOF_VALIDATOR_EL", "op-reth")
	}

	runtime := sysgo.NewMixedSingleChainRuntime(t, sysgo.MixedSingleChainPresetConfig{
		NodeSpecs: []sysgo.MixedSingleChainNodeSpec{
			{
				ELKey:       "sequencer",
				CLKey:       "sequencer",
				ELKind:      resolveELKind("OP_DEVSTACK_PROOF_SEQUENCER_EL", sysgo.MixedL2ELOpReth),
				CLKind:      sysgo.MixedL2CLOpNode,
				IsSequencer: true,
			},
			{
				ELKey:       "validator",
				CLKey:       "validator",
				ELKind:      resolveELKind("OP_DEVSTACK_PROOF_VALIDATOR_EL", sysgo.MixedL2ELOpReth),
				CLKind:      sysgo.MixedL2CLOpNode,
				IsSequencer: false,
			},
		},
	})

	frontends := presets.NewMixedSingleChainFrontends(t, runtime)

	var seqEL *dsl.L2ELNode
	for i := range frontends.Nodes {
		if frontends.Nodes[i].Spec.IsSequencer {
			seqEL = frontends.Nodes[i].EL
			break
		}
	}
	t.Require().NotNil(seqEL, "sequencer EL not found in frontends")

	wallet := dsl.NewRandomHDWallet(t, 30)
	funder := dsl.NewFunder(wallet, frontends.FaucetL2, seqEL)
	eoa := funder.NewFundedEOA(eth.OneEther)
	privHex := hex.EncodeToString(crypto.FromECDSA(eoa.Key().Priv()))

	line := "======================================================================"
	fmt.Println(line)
	fmt.Println("XLAYER AA DEVNET READY — press Ctrl+C to tear down")
	fmt.Println(line)
	fmt.Printf("L1 chain id          %s\n", runtime.L1Network.ChainID())
	fmt.Printf("L1 EL user RPC       %s\n", runtime.L1EL.UserRPC())
	fmt.Printf("L2 chain id          %s\n", runtime.L2Network.ChainID())
	for _, n := range runtime.Nodes {
		fmt.Printf("L2 %-10s EL UserRPC   %s\n", n.Spec.ELKey, n.EL.UserRPC())
		fmt.Printf("L2 %-10s EL EngineRPC %s\n", n.Spec.ELKey, n.EL.EngineRPC())
		fmt.Printf("L2 %-10s CL UserRPC   %s\n", n.Spec.CLKey, n.CL.UserRPC())
	}
	fmt.Printf("Funded EOA address   %s\n", eoa.Address().Hex())
	fmt.Printf("Funded EOA priv hex  0x%s\n", privHex)
	fmt.Println(line)

	sigCh := make(chan os.Signal, 1)
	signal.Notify(sigCh, syscall.SIGINT, syscall.SIGTERM)
	select {
	case <-sigCh:
		t.Logger().Info("received signal, tearing down devnet")
	case <-t.Ctx().Done():
		t.Logger().Info("test context cancelled, tearing down devnet")
	}
}

func resolveELKind(envVar string, fallback sysgo.MixedL2ELKind) sysgo.MixedL2ELKind {
	switch os.Getenv(envVar) {
	case "op-reth", "op-reth-with-proof":
		return sysgo.MixedL2ELOpReth
	case "op-geth":
		return sysgo.MixedL2ELOpGeth
	default:
		return fallback
	}
}
