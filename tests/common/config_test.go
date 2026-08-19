package common

import (
	"path/filepath"
	"strings"
	"testing"
)

func TestValidateRejectsEmptyOptimismRoot(t *testing.T) {
	cfg := &XLayerConfig{OptimismRoot: ""}
	err := cfg.Validate()
	if err == nil {
		t.Fatal("expected empty OPTIMISM_ROOT to be rejected")
	}
	if !strings.Contains(err.Error(), EnvOptimismRoot) {
		t.Fatalf("error must name the variable %q; got %v", EnvOptimismRoot, err)
	}
}

func TestValidateRejectsRelativeOptimismRoot(t *testing.T) {
	cfg := &XLayerConfig{OptimismRoot: "deps/optimism"}
	err := cfg.Validate()
	if err == nil {
		t.Fatal("expected relative OPTIMISM_ROOT to be rejected")
	}
	if !strings.Contains(err.Error(), "absolute") {
		t.Fatalf("error must explain the absolute-path constraint; got %v", err)
	}
}

func TestValidateAcceptsAbsoluteOptimismRoot(t *testing.T) {
	cfg := &XLayerConfig{OptimismRoot: string(filepath.Separator) + filepath.Join("repo", "deps", "optimism")}
	if err := cfg.Validate(); err != nil {
		t.Fatalf("absolute OPTIMISM_ROOT should validate; got %v", err)
	}
}

func TestRequireExecutionBinary(t *testing.T) {
	abs := string(filepath.Separator) + filepath.Join("repo", "deps", "optimism")
	cfg := &XLayerConfig{OptimismRoot: abs}
	if err := cfg.RequireExecutionBinary(); err == nil {
		t.Fatal("expected missing reth binary to be rejected")
	}
	cfg.RethExecutionBinary = "relative/reth"
	if err := cfg.RequireExecutionBinary(); err == nil {
		t.Fatal("expected relative reth binary to be rejected")
	}
	cfg.RethExecutionBinary = string(filepath.Separator) + filepath.Join("repo", "target", "debug", "xlayer-reth-node")
	if err := cfg.RequireExecutionBinary(); err != nil {
		t.Fatalf("absolute reth binary should validate; got %v", err)
	}
}

func TestDerivedContractPaths(t *testing.T) {
	root := string(filepath.Separator) + filepath.Join("opt", "optimism")
	cfg := &XLayerConfig{OptimismRoot: root}
	wantBedrock := filepath.Join(root, "packages", "contracts-bedrock")
	if got := cfg.ContractsBedrockDir(); got != wantBedrock {
		t.Fatalf("ContractsBedrockDir = %q, want %q", got, wantBedrock)
	}
	wantArtifacts := filepath.Join(wantBedrock, "forge-artifacts")
	if got := cfg.ForgeArtifactsDir(); got != wantArtifacts {
		t.Fatalf("ForgeArtifactsDir = %q, want %q", got, wantArtifacts)
	}
	if !strings.HasSuffix(cfg.GaslessDeployScript(), filepath.Join("scripts", "deploy", "DeployXlayerGaslessWhitelist.s.sol")) {
		t.Fatalf("GaslessDeployScript has unexpected suffix: %q", cfg.GaslessDeployScript())
	}
}

func TestConsensusClientIsFixed(t *testing.T) {
	cfg := &XLayerConfig{OptimismRoot: string(filepath.Separator) + "opt"}
	if cfg.ConsensusClient() != "op-node" {
		t.Fatalf("consensus client must be fixed to op-node; got %q", cfg.ConsensusClient())
	}
}

func TestLoadReadsEnvOnce(t *testing.T) {
	abs := string(filepath.Separator) + filepath.Join("srv", "optimism")
	t.Setenv(EnvOptimismRoot, abs)
	t.Setenv(EnvRethExecutionBinary, "")
	cfg, err := LoadXLayerConfig()
	if err != nil {
		t.Fatalf("load with absolute root should succeed; got %v", err)
	}
	if cfg.OptimismRoot != abs {
		t.Fatalf("OptimismRoot = %q, want %q", cfg.OptimismRoot, abs)
	}
	paths := cfg.AuditablePaths()
	if paths[EnvOptimismRoot] != abs {
		t.Fatalf("AuditablePaths missing OPTIMISM_ROOT; got %v", paths)
	}
	if _, ok := paths[EnvRethExecutionBinary]; !ok {
		t.Fatalf("AuditablePaths must list %s", EnvRethExecutionBinary)
	}
}

func TestAmountHelpersReuseOpService(t *testing.T) {
	if GWei(1).String() == "" {
		t.Fatal("GWei should produce a non-empty wei value via op-service/eth")
	}
	if Ether(1).String() == "" {
		t.Fatal("Ether should produce a non-empty wei value via op-service/eth")
	}
}
