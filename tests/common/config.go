package common

import (
	"fmt"
	"os"
	"path/filepath"

	"github.com/ethereum-optimism/optimism/op-devstack/sysgo"
)

// Environment variable names consumed by the harness. They are declared here so
// the external configuration surface can be audited from a single location.
const (
	// EnvOptimismRoot locates the Optimism monorepo checkout that supplies the
	// contracts-bedrock artifacts and devstack packages used to build a devnet.
	EnvOptimismRoot = "OPTIMISM_ROOT"
	// EnvRethExecutionBinary locates the prebuilt XLayer reth execution client
	// used as the L2 execution layer for every devnet node. This is the same
	// variable op-devstack's rustbin reads to run a prebuilt op-reth binary
	// instead of building it, so one value both configures the harness and
	// selects the node binary.
	EnvRethExecutionBinary = "RUST_BINARY_PATH_OP_RETH"
	// EnvConsensusClient is the upstream op-devstack selector for the L2
	// consensus client. An empty value defaults to op-node.
	EnvConsensusClient = "DEVSTACK_L2CL_KIND"
)

// ConsensusClient identifies an L2 consensus client supported by the XLayer
// E2E harness. It aliases op-devstack's selector type so the harness and runtime
// cannot drift onto different names.
type ConsensusClient = sysgo.MixedL2CLKind

const (
	ConsensusClientOpNode = sysgo.MixedL2CLOpNode
	ConsensusClientKona   = sysgo.MixedL2CLKona
)

// XLayerConfig is the single source of external run configuration for the XLayer
// Go E2E harness. Every consumer (scenarios, devstack glue, RPC/sync helpers)
// receives a populated *XLayerConfig instead of reading process environment
// itself; this type and its loader are the only place environment variables are
// read, defaulted and validated.
type XLayerConfig struct {
	// OptimismRoot is the absolute path to the Optimism monorepo checkout.
	OptimismRoot string
	// RethExecutionBinary is the absolute path to the prebuilt XLayer reth
	// execution client run as the L2 EL. Empty is permitted at load time so
	// callers that only need contract paths can load without a built binary;
	// RequireExecutionBinary enforces its presence for scenarios that start a
	// devnet.
	RethExecutionBinary string
	// L2ConsensusClient selects the consensus client used for every XLayer L2
	// node. LoadXLayerConfig obtains it from DEVSTACK_L2CL_KIND.
	L2ConsensusClient ConsensusClient
}

// LoadXLayerConfig reads every harness environment variable exactly once,
// applies defaults, validates the result, and returns an immutable config
// object. It is the only environment entry point in the harness.
func LoadXLayerConfig() (*XLayerConfig, error) {
	consensusClient := ConsensusClient(os.Getenv(EnvConsensusClient))
	if consensusClient == "" {
		consensusClient = ConsensusClientOpNode
	}
	cfg := &XLayerConfig{
		OptimismRoot:        os.Getenv(EnvOptimismRoot),
		RethExecutionBinary: os.Getenv(EnvRethExecutionBinary),
		L2ConsensusClient:   consensusClient,
	}
	if err := cfg.Validate(); err != nil {
		return nil, err
	}
	return cfg, nil
}

// Validate rejects any configuration that would let a devnet start with an
// ambiguous external root. It returns an error before any process is spawned so
// misconfiguration surfaces as a clear message rather than a downstream devnet
// failure.
func (c *XLayerConfig) Validate() error {
	if err := c.validateOptimismRoot(); err != nil {
		return err
	}
	return c.validateConsensusClient()
}

func (c *XLayerConfig) validateConsensusClient() error {
	switch c.ConsensusClient() {
	case ConsensusClientOpNode, ConsensusClientKona:
		return nil
	default:
		return fmt.Errorf("%s must be %q or %q; got %q", EnvConsensusClient, ConsensusClientOpNode, ConsensusClientKona, c.L2ConsensusClient)
	}
}

// validateOptimismRoot enforces that OPTIMISM_ROOT is a non-empty absolute path.
// A relative path is rejected because the devnet resolves contract artifacts
// from this root while running with a working directory that differs per test
// process, so a relative value would resolve inconsistently.
func (c *XLayerConfig) validateOptimismRoot() error {
	if c.OptimismRoot == "" {
		return fmt.Errorf("%s must be set to the absolute Optimism monorepo root; it was empty", EnvOptimismRoot)
	}
	if !filepath.IsAbs(c.OptimismRoot) {
		return fmt.Errorf("%s must be an absolute path; got relative path %q", EnvOptimismRoot, c.OptimismRoot)
	}
	return nil
}

// RequireExecutionBinary returns an error unless a concrete XLayer reth binary
// path is configured. Scenarios that start a devnet call this so a missing
// execution client fails fast with the variable name rather than deep inside
// node startup.
func (c *XLayerConfig) RequireExecutionBinary() error {
	if c.RethExecutionBinary == "" {
		return fmt.Errorf("%s must point to a prebuilt XLayer reth execution binary; it was empty", EnvRethExecutionBinary)
	}
	if !filepath.IsAbs(c.RethExecutionBinary) {
		return fmt.Errorf("%s must be an absolute path; got relative path %q", EnvRethExecutionBinary, c.RethExecutionBinary)
	}
	return nil
}

// ContractsBedrockDir is the contracts-bedrock package directory under the
// configured Optimism root.
func (c *XLayerConfig) ContractsBedrockDir() string {
	return filepath.Join(c.OptimismRoot, "packages", "contracts-bedrock")
}

// ForgeArtifactsDir is the compiled Forge artifact directory the devnet reads
// genesis and deploy inputs from.
func (c *XLayerConfig) ForgeArtifactsDir() string {
	return filepath.Join(c.ContractsBedrockDir(), "forge-artifacts")
}

// GaslessDeployScript is the path to the XLayer gasless-whitelist deploy script
// invoked by the gasless scenarios.
func (c *XLayerConfig) GaslessDeployScript() string {
	return filepath.Join(c.ContractsBedrockDir(), "scripts", "deploy", "DeployXlayerGaslessWhitelist.s.sol")
}

// ConsensusClient reports the selected L2 consensus client, defaulting to
// op-node for directly constructed zero-value configurations.
func (c *XLayerConfig) ConsensusClient() ConsensusClient {
	if c.L2ConsensusClient == "" {
		return ConsensusClientOpNode
	}
	return c.L2ConsensusClient
}

// AuditablePaths returns the complete set of external path inputs keyed by their
// environment variable name, so an operator can inspect every path the harness
// depends on from one call.
func (c *XLayerConfig) AuditablePaths() map[string]string {
	return map[string]string{
		EnvOptimismRoot:        c.OptimismRoot,
		EnvRethExecutionBinary: c.RethExecutionBinary,
	}
}
