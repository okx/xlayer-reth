package common

import (
	"context"
	"encoding/json"
	"fmt"
	"math/big"

	"github.com/ethereum/go-ethereum"
	"github.com/ethereum/go-ethereum/common"
	"github.com/ethereum/go-ethereum/common/hexutil"
	"github.com/ethereum/go-ethereum/core/types"
	"github.com/ethereum/go-ethereum/ethclient"
	"github.com/ethereum/go-ethereum/rpc"
)

// BlockTag names a canonical block selector understood by the JSON-RPC block
// parameter. It mirrors the Rust harness's BlockId {Latest, Pending, Number, Hash}
// distinction so scenarios can query pending vs latest state explicitly.
type BlockTag string

const (
	// BlockLatest selects the latest canonical block.
	BlockLatest BlockTag = "latest"
	// BlockPending selects pending (including flashblock) state.
	BlockPending BlockTag = "pending"
)

// DevnetClient is the JSON-RPC handle a scenario uses to talk to one devnet node.
// It owns both a typed ethclient for standard calls and a raw RPC client for the
// methods (debug tracing, txpool, simulate, block receipts) that have no typed
// wrapper, mirroring the Rust operations RPC helper layer.
type DevnetClient struct {
	url string
	rpc *rpc.Client
	eth *ethclient.Client
}

// NewDevnetClient dials url and returns a client that shares one connection
// between the typed and raw RPC layers.
func NewDevnetClient(ctx context.Context, url string) (*DevnetClient, error) {
	rc, err := rpc.DialContext(ctx, url)
	if err != nil {
		return nil, fmt.Errorf("dial %s: %w", url, err)
	}
	return &DevnetClient{url: url, rpc: rc, eth: ethclient.NewClient(rc)}, nil
}

// URL returns the endpoint this client is bound to.
func (c *DevnetClient) URL() string { return c.url }

// Eth exposes the underlying typed client for callers that need more of its API.
func (c *DevnetClient) Eth() *ethclient.Client { return c.eth }

// Close releases the underlying connection.
func (c *DevnetClient) Close() {
	if c.rpc != nil {
		c.rpc.Close()
	}
}

// ChainID returns eth_chainId.
func (c *DevnetClient) ChainID(ctx context.Context) (*big.Int, error) {
	return c.eth.ChainID(ctx)
}

// BlockNumber returns eth_blockNumber.
func (c *DevnetClient) BlockNumber(ctx context.Context) (uint64, error) {
	return c.eth.BlockNumber(ctx)
}

// BalanceAt returns eth_getBalance at the latest block.
func (c *DevnetClient) BalanceAt(ctx context.Context, addr common.Address) (*big.Int, error) {
	return c.eth.BalanceAt(ctx, addr, nil)
}

// CodeAt returns eth_getCode at the latest block.
func (c *DevnetClient) CodeAt(ctx context.Context, addr common.Address) ([]byte, error) {
	return c.eth.CodeAt(ctx, addr, nil)
}

// NonceAt returns eth_getTransactionCount at the latest block.
func (c *DevnetClient) NonceAt(ctx context.Context, addr common.Address) (uint64, error) {
	return c.eth.NonceAt(ctx, addr, nil)
}

// StorageAt returns eth_getStorageAt for the given slot at the latest block.
func (c *DevnetClient) StorageAt(ctx context.Context, addr common.Address, slot common.Hash) ([]byte, error) {
	return c.eth.StorageAt(ctx, addr, slot, nil)
}

// SuggestGasPrice returns eth_gasPrice.
func (c *DevnetClient) SuggestGasPrice(ctx context.Context) (*big.Int, error) {
	return c.eth.SuggestGasPrice(ctx)
}

// TransactionByHash returns eth_getTransactionByHash.
func (c *DevnetClient) TransactionByHash(ctx context.Context, hash common.Hash) (*types.Transaction, bool, error) {
	return c.eth.TransactionByHash(ctx, hash)
}

// TransactionReceipt returns eth_getTransactionReceipt.
func (c *DevnetClient) TransactionReceipt(ctx context.Context, hash common.Hash) (*types.Receipt, error) {
	return c.eth.TransactionReceipt(ctx, hash)
}

// BlockByNumber returns eth_getBlockByNumber; a nil number selects the latest block.
func (c *DevnetClient) BlockByNumber(ctx context.Context, number *big.Int) (*types.Block, error) {
	return c.eth.BlockByNumber(ctx, number)
}

// SendTransaction submits a signed transaction via eth_sendRawTransaction.
func (c *DevnetClient) SendTransaction(ctx context.Context, tx *types.Transaction) error {
	return c.eth.SendTransaction(ctx, tx)
}

// FilterLogs returns eth_getLogs for the given query.
func (c *DevnetClient) FilterLogs(ctx context.Context, q ethereum.FilterQuery) ([]types.Log, error) {
	return c.eth.FilterLogs(ctx, q)
}

// CallContract executes eth_call against the given message at the latest block.
func (c *DevnetClient) CallContract(ctx context.Context, msg ethereum.CallMsg) ([]byte, error) {
	return c.eth.CallContract(ctx, msg, nil)
}

// EstimateGas returns eth_estimateGas for the given message.
func (c *DevnetClient) EstimateGas(ctx context.Context, msg ethereum.CallMsg) (uint64, error) {
	return c.eth.EstimateGas(ctx, msg)
}

// RawCall dispatches an arbitrary JSON-RPC method, decoding the result into out.
// It backs the helpers below that the typed client does not expose.
func (c *DevnetClient) RawCall(ctx context.Context, out any, method string, args ...any) error {
	return c.rpc.CallContext(ctx, out, method, args...)
}

// SuggestFeeCaps returns a priority tip and a fee cap derived from the current
// suggested gas price: tip equal to the gas price and a fee cap of twice it, so
// ordinary (non-gasless) transactions are accepted.
func (c *DevnetClient) SuggestFeeCaps(ctx context.Context) (tip, feeCap *big.Int, err error) {
	gp, err := c.SuggestGasPrice(ctx)
	if err != nil {
		return nil, nil, err
	}
	return new(big.Int).Set(gp), new(big.Int).Mul(gp, big.NewInt(2)), nil
}

// BlockReceiptsByNumber returns eth_getBlockReceipts for a block tag or number.
func (c *DevnetClient) BlockReceiptsByNumber(ctx context.Context, block string) ([]json.RawMessage, error) {
	var out []json.RawMessage
	if err := c.rpc.CallContext(ctx, &out, "eth_getBlockReceipts", block); err != nil {
		return nil, err
	}
	return out, nil
}

// BlockReceiptsByHash returns eth_getBlockReceipts for a block hash.
func (c *DevnetClient) BlockReceiptsByHash(ctx context.Context, hash common.Hash) ([]json.RawMessage, error) {
	var out []json.RawMessage
	if err := c.rpc.CallContext(ctx, &out, "eth_getBlockReceipts", hash.Hex()); err != nil {
		return nil, err
	}
	return out, nil
}

// LogsByBlockHash returns eth_getLogs filtered by an explicit blockHash. It is the
// routing regression the Rust harness checks: an unknown-address filter must
// return an empty array rather than a "block not found" error.
func (c *DevnetClient) LogsByBlockHash(ctx context.Context, blockHash common.Hash, addr common.Address) ([]types.Log, error) {
	filter := map[string]any{
		"blockHash": blockHash.Hex(),
		"address":   addr.Hex(),
	}
	var out []types.Log
	if err := c.rpc.CallContext(ctx, &out, "eth_getLogs", filter); err != nil {
		return nil, err
	}
	return out, nil
}

// TxpoolContent returns txpool_content.
func (c *DevnetClient) TxpoolContent(ctx context.Context) (map[string]any, error) {
	var out map[string]any
	if err := c.rpc.CallContext(ctx, &out, "txpool_content"); err != nil {
		return nil, err
	}
	return out, nil
}

// TxpoolStatus returns txpool_status.
func (c *DevnetClient) TxpoolStatus(ctx context.Context) (map[string]any, error) {
	var out map[string]any
	if err := c.rpc.CallContext(ctx, &out, "txpool_status"); err != nil {
		return nil, err
	}
	return out, nil
}

// DebugTraceTransaction returns debug_traceTransaction with the default struct
// logger (empty tracer config), matching the Rust harness.
func (c *DevnetClient) DebugTraceTransaction(ctx context.Context, hash common.Hash) (json.RawMessage, error) {
	var out json.RawMessage
	if err := c.rpc.CallContext(ctx, &out, "debug_traceTransaction", hash.Hex(), map[string]any{}); err != nil {
		return nil, err
	}
	return out, nil
}

// DebugTraceBlockByHash returns debug_traceBlockByHash with the default tracer.
func (c *DevnetClient) DebugTraceBlockByHash(ctx context.Context, hash common.Hash) (json.RawMessage, error) {
	var out json.RawMessage
	if err := c.rpc.CallContext(ctx, &out, "debug_traceBlockByHash", hash.Hex(), map[string]any{}); err != nil {
		return nil, err
	}
	return out, nil
}

// DebugTraceBlockByNumber returns debug_traceBlockByNumber with the default tracer.
func (c *DevnetClient) DebugTraceBlockByNumber(ctx context.Context, number uint64) (json.RawMessage, error) {
	var out json.RawMessage
	if err := c.rpc.CallContext(ctx, &out, "debug_traceBlockByNumber", hexutil.EncodeUint64(number), map[string]any{}); err != nil {
		return nil, err
	}
	return out, nil
}

// RawEthCall executes eth_call against an arbitrary call object at the given block
// tag, returning the raw hex result. It is used by the gasless scenarios, whose
// zero-priced call object must not be rejected for insufficient base fee.
func (c *DevnetClient) RawEthCall(ctx context.Context, callObj map[string]any, tag BlockTag) (hexutil.Bytes, error) {
	var out hexutil.Bytes
	if err := c.rpc.CallContext(ctx, &out, "eth_call", callObj, string(tag)); err != nil {
		return nil, err
	}
	return out, nil
}

// SimulateV1 executes eth_simulateV1 with the given payload at the block tag.
func (c *DevnetClient) SimulateV1(ctx context.Context, payload map[string]any, tag BlockTag) (json.RawMessage, error) {
	var out json.RawMessage
	if err := c.rpc.CallContext(ctx, &out, "eth_simulateV1", payload, string(tag)); err != nil {
		return nil, err
	}
	return out, nil
}

// PendingTransactionCount returns eth_getBlockTransactionCountByNumber for the
// pending tag, used by the flashblocks pending-tag smoke coverage.
func (c *DevnetClient) BlockTransactionCount(ctx context.Context, tag BlockTag) (uint64, error) {
	var out hexutil.Uint64
	if err := c.rpc.CallContext(ctx, &out, "eth_getBlockTransactionCountByNumber", string(tag)); err != nil {
		return 0, err
	}
	return uint64(out), nil
}
