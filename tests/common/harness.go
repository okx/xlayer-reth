package common

import (
	"math/big"
	"os"
	"path/filepath"
	"testing"

	"github.com/ethereum-optimism/optimism/op-devstack/devtest"
	"github.com/ethereum-optimism/optimism/op-devstack/dsl"
	"github.com/ethereum-optimism/optimism/op-devstack/presets"
	"github.com/ethereum-optimism/optimism/op-service/eth"
)

// funderFundingAmount is the balance granted to a scenario's sender account.
var funderFundingAmount = eth.Ether(1000)

// findRepoRoot walks up from the current working directory until it finds the
// xlayer-reth checkout root (identified by its Cargo.toml). It reads the
// filesystem only, never the process environment.
func findRepoRoot(t *testing.T) string {
	t.Helper()
	dir, err := os.Getwd()
	if err != nil {
		t.Fatalf("getwd: %v", err)
	}
	for {
		if _, err := os.Stat(filepath.Join(dir, "Cargo.toml")); err == nil {
			return dir
		}
		parent := filepath.Dir(dir)
		if parent == dir {
			t.Fatalf("could not locate repo root (Cargo.toml) above %s", dir)
		}
		dir = parent
	}
}

// SingleChainSystem is a started XLayer single-chain devnet plus the sequencer
// client and a funded sender that a scenario drives.
type SingleChainSystem struct {
	T      *testing.T
	DT     devtest.T
	Preset *presets.XLayerSingleChain
	Config XLayerConfig
	Seq    *DevnetClient
	Sender *Account
}

// StartSingleChain brings up the XLayer single-chain devnet and returns a ready
// system. It skips the test when the XLayer Reth binary has not been built in
// the current environment, so compiled scenarios never fail merely because the
// devnet cannot run here.
func StartSingleChain(t *testing.T) *SingleChainSystem {
	cfg := loadConfigOrSkip(t)
	dt := devtest.SerialT(t)
	preset := presets.NewXLayerSingleChain(dt)

	seq := dialOrFatal(t, dt, preset.L2EL.Escape().UserRPC())
	chainID, err := seq.ChainID(dt.Ctx())
	if err != nil {
		t.Fatalf("chain id: %v", err)
	}
	sender := fundSender(t, dt, preset.FunderL2, chainID)

	return &SingleChainSystem{T: t, DT: dt, Preset: preset, Config: cfg, Seq: seq, Sender: sender}
}

// FlashblocksSystem is a started XLayer Flashblocks devnet plus the producer
// client and a funded sender.
type FlashblocksSystem struct {
	T      *testing.T
	DT     devtest.T
	Preset *presets.XLayerFlashblocks
	Config XLayerConfig
	// Producer is the RPC client for the sequencer/producer node.
	Producer *Account
	Client   *DevnetClient
}

// StartFlashblocks brings up the XLayer Flashblocks devnet and returns a ready
// system, skipping when the XLayer Reth binary is unavailable.
func StartFlashblocks(t *testing.T) *FlashblocksSystem {
	cfg := loadConfigOrSkip(t)
	dt := devtest.SerialT(t)
	preset := presets.NewXLayerFlashblocks(dt)

	client := dialOrFatal(t, dt, preset.L2EL.Escape().UserRPC())
	chainID, err := client.ChainID(dt.Ctx())
	if err != nil {
		t.Fatalf("chain id: %v", err)
	}
	sender := fundSender(t, dt, preset.FunderL2, chainID)

	return &FlashblocksSystem{T: t, DT: dt, Preset: preset, Config: cfg, Producer: sender, Client: client}
}

// loadConfigOrSkip loads the central config and skips the test when the devnet
// execution client is not available on disk.
func loadConfigOrSkip(t *testing.T) XLayerConfig {
	cfg, err := LoadXLayerConfig(findRepoRoot(t))
	if err != nil {
		t.Fatalf("load config: %v", err)
	}
	if !cfg.ExecutionClientReady() {
		t.Skipf("XLayer Reth execution client not built at %s; build it (cargo build -p xlayer-reth-node) to run this devnet scenario", cfg.XLayerRethBin)
	}
	return cfg
}

// fundSender asks the devnet funder for a freshly funded EOA and adapts its
// runtime-generated key into a raw-signing Account. No private key is ever
// hard-coded: the funder mints the account on the running devnet.
func fundSender(t *testing.T, dt devtest.T, funder *dsl.Funder, chainID *big.Int) *Account {
	t.Helper()
	eoa := funder.NewFundedEOA(funderFundingAmount)
	return NewAccount(eoa.Key().Priv(), chainID)
}

// dialOrFatal dials a node URL and fails the test on error.
func dialOrFatal(t *testing.T, dt devtest.T, url string) *DevnetClient {
	c, err := NewDevnetClient(dt.Ctx(), url)
	if err != nil {
		t.Fatalf("dial %s: %v", url, err)
	}
	dt.Cleanup(c.Close)
	return c
}
