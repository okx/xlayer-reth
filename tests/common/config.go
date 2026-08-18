package common

import (
	"fmt"
	"os"
	"path/filepath"
	"time"
)

// Environment variable names consumed by the harness. They are declared here,
// read exactly once in LoadXLayerConfig, and never accessed anywhere else in the
// tests module or in the XLayer devstack code.
const (
	// EnvOptimismRoot points at the Optimism root that supplies devnet contract
	// artifacts and the devstack. Accepts the in-repo default absolute path or a
	// caller-supplied external absolute path; empty or relative values are
	// rejected before any devnet process starts.
	EnvOptimismRoot = "OPTIMISM_ROOT"
	// EnvXLayerRethBin points at the pre-built XLayer Reth execution-client binary
	// the devnet runs as its L2 EL. When unset the harness derives the repo-default
	// debug build path.
	EnvXLayerRethBin = "XLAYER_RETH_BIN"
	// EnvPerTestTimeout overrides the hard per-test-process timeout.
	EnvPerTestTimeout = "XLAYER_E2E_TEST_TIMEOUT"
	// EnvGaslessContract is the address of the XLayer gasless predeploy that owns
	// the enable/whitelist API. It is chain-configuration specific and has no
	// default, so gasless scenarios skip cleanly until it is provided.
	EnvGaslessContract = "XLAYER_GASLESS_CONTRACT"
)

// DefaultPerTestTimeout is the hard timeout for a single test process.
const DefaultPerTestTimeout = 5 * time.Minute

// XLayerConfig is the single configuration object the harness passes explicitly
// to every consumer. It is produced once by LoadXLayerConfig; no other code path
// reads the process environment.
type XLayerConfig struct {
	// OptimismRoot is the validated absolute path to the Optimism root.
	OptimismRoot string
	// XLayerRethBin is the absolute path to the XLayer Reth execution-client binary.
	XLayerRethBin string
	// PerTestTimeout is the hard per-test-process timeout.
	PerTestTimeout time.Duration
	// GaslessContract is the optional gasless predeploy address (empty when unset).
	GaslessContract string
}

// LoadXLayerConfig is the ONLY place in the tests module that reads process
// environment. It declares, reads, defaults, and validates every variable once,
// then hands the resulting XLayerConfig to all consumers. repoRoot is the
// absolute path to the xlayer-reth checkout, used to derive in-repo defaults.
func LoadXLayerConfig(repoRoot string) (XLayerConfig, error) {
	cfg := XLayerConfig{PerTestTimeout: DefaultPerTestTimeout}

	optimismRoot := os.Getenv(EnvOptimismRoot)
	if optimismRoot == "" {
		optimismRoot = filepath.Join(repoRoot, "deps", "optimism")
	}
	if !filepath.IsAbs(optimismRoot) {
		return XLayerConfig{}, fmt.Errorf("%s must be an absolute path, got %q", EnvOptimismRoot, optimismRoot)
	}
	cfg.OptimismRoot = optimismRoot

	rethBin := os.Getenv(EnvXLayerRethBin)
	if rethBin == "" {
		rethBin = filepath.Join(repoRoot, "target", "debug", "xlayer-reth-node")
	}
	if !filepath.IsAbs(rethBin) {
		return XLayerConfig{}, fmt.Errorf("%s must be an absolute path, got %q", EnvXLayerRethBin, rethBin)
	}
	cfg.XLayerRethBin = rethBin

	if raw := os.Getenv(EnvPerTestTimeout); raw != "" {
		d, err := time.ParseDuration(raw)
		if err != nil {
			return XLayerConfig{}, fmt.Errorf("%s must be a Go duration, got %q: %w", EnvPerTestTimeout, raw, err)
		}
		cfg.PerTestTimeout = d
	}

	cfg.GaslessContract = os.Getenv(EnvGaslessContract)

	return cfg, nil
}

// GaslessReady reports whether a gasless predeploy address has been configured,
// so gasless scenarios can skip cleanly when it is absent.
func (c XLayerConfig) GaslessReady() bool {
	return c.GaslessContract != ""
}

// ExecutionClientReady reports whether the configured XLayer Reth binary exists
// on disk, so scenarios can skip cleanly when the devnet cannot be started in the
// current environment.
func (c XLayerConfig) ExecutionClientReady() bool {
	info, err := os.Stat(c.XLayerRethBin)
	return err == nil && !info.IsDir()
}
