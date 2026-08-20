package rpc

import (
	"encoding/json"
	"io"
	"net/http"
	"net/http/httptest"
	"strconv"
	"strings"
	"sync"
	"testing"

	"github.com/ethereum-optimism/optimism/op-devstack/devtest"
	"github.com/ethereum-optimism/optimism/op-devstack/presets"
	"github.com/ethereum-optimism/optimism/op-devstack/sysgo"
	"github.com/ethereum/go-ethereum/common/hexutil"
)

type legacySentinel struct {
	mu       sync.Mutex
	hits     int
	requests []legacyRequest
	errors   []string
}

type legacyRequest struct {
	Method string            `json:"method"`
	Params []json.RawMessage `json:"params"`
	ID     json.RawMessage   `json:"id"`
}

func (s *legacySentinel) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	var request legacyRequest
	if err := json.NewDecoder(r.Body).Decode(&request); err != nil {
		s.recordError(err.Error())
		if writeErr := writeJSON(w, http.StatusBadRequest, map[string]any{
			"jsonrpc": "2.0",
			"id":      1,
			"error": map[string]any{
				"code":    -32700,
				"message": "invalid legacy sentinel request",
			},
		}); writeErr != nil {
			s.recordError(writeErr.Error())
		}
		return
	}

	s.mu.Lock()
	s.hits++
	s.requests = append(s.requests, request)
	s.mu.Unlock()

	var result any
	if request.Method == "eth_getBlockByNumber" {
		result = legacyBlockResult()
	}
	if err := writeJSON(w, http.StatusOK, map[string]any{
		"jsonrpc": "2.0",
		// The legacy middleware always forwards with its own JSON-RPC ID 1 and
		// restores the caller's ID when returning the result.
		"id":     1,
		"result": result,
	}); err != nil {
		s.recordError(err.Error())
	}
}

func legacyBlockResult() map[string]any {
	return map[string]any{
		"number":         "0x1",
		"hash":           "0x" + strings.Repeat("11", 32),
		"legacySentinel": true,
	}
}

func TestLegacySentinelResponseIsValidJSON(gt *testing.T) {
	sentinel := new(legacySentinel)
	server := httptest.NewServer(sentinel)
	defer server.Close()

	httpResponse, err := http.Post(server.URL, "application/json", strings.NewReader(
		`{"jsonrpc":"2.0","method":"eth_getBlockByNumber","params":["0x1",false],"id":1}`,
	))
	if err != nil {
		gt.Fatalf("request legacy sentinel: %v", err)
	}
	defer httpResponse.Body.Close()
	payload, err := io.ReadAll(httpResponse.Body)
	if err != nil {
		gt.Fatalf("read legacy sentinel response: %v", err)
	}
	if httpResponse.StatusCode != http.StatusOK {
		gt.Fatalf("unexpected status %d: %s", httpResponse.StatusCode, payload)
	}
	if httpResponse.ContentLength != int64(len(payload)) {
		gt.Fatalf("unexpected content length %d for %d-byte body", httpResponse.ContentLength, len(payload))
	}

	var rpcResponse struct {
		JSONRPC string          `json:"jsonrpc"`
		ID      int             `json:"id"`
		Result  json.RawMessage `json:"result"`
	}
	if err := json.Unmarshal(payload, &rpcResponse); err != nil {
		gt.Fatalf("legacy response is not valid JSON: %v", err)
	}
	if rpcResponse.JSONRPC != "2.0" || rpcResponse.ID != 1 {
		gt.Fatalf("unexpected JSON-RPC envelope: jsonrpc=%q id=%d", rpcResponse.JSONRPC, rpcResponse.ID)
	}

	var block map[string]json.RawMessage
	if err := json.Unmarshal(rpcResponse.Result, &block); err != nil {
		gt.Fatalf("legacy result is not a JSON object: %v", err)
	}
	if string(block["legacySentinel"]) != "true" {
		gt.Fatalf("legacy sentinel marker missing from response: %s", rpcResponse.Result)
	}
}

func writeJSON(w http.ResponseWriter, status int, value any) error {
	payload, err := json.Marshal(value)
	if err != nil {
		http.Error(w, "failed to encode legacy sentinel response", http.StatusInternalServerError)
		return err
	}
	w.Header().Set("Content-Type", "application/json")
	w.Header().Set("Content-Length", strconv.Itoa(len(payload)))
	w.Header().Set("Connection", "close")
	w.WriteHeader(status)
	_, err = w.Write(payload)
	return err
}

func (s *legacySentinel) recordError(message string) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.errors = append(s.errors, message)
}

func (s *legacySentinel) snapshot() (int, []legacyRequest, []string) {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.hits, append([]legacyRequest(nil), s.requests...), append([]string(nil), s.errors...)
}

func requestsForMethod(requests []legacyRequest, method string) []legacyRequest {
	matched := make([]legacyRequest, 0, 1)
	for _, request := range requests {
		if request.Method == method {
			matched = append(matched, request)
		}
	}
	return matched
}

func startLegacySentinel(t devtest.T) (*legacySentinel, string) {
	sentinel := new(legacySentinel)
	server := httptest.NewServer(sentinel)
	t.Cleanup(server.Close)
	return sentinel, server.URL
}

// assertLegacyRPCRouting uses the shared TestRPC topology. Blocks below the
// non-zero L2 genesis height must be forwarded, while genesis and dynamic tags
// remain served by the local node.
func assertLegacyRPCRouting(t devtest.T, sys *presets.XLayer, sentinel *legacySentinel) {
	rpc := sys.L2EL.EthClient().RPC()

	genesisHeight := sysgo.XLayerDefaultL2GenesisHeight
	preGenesis := hexutil.EncodeUint64(genesisHeight - 1)
	var legacyBlock map[string]json.RawMessage
	err := rpc.CallContext(t.Ctx(), &legacyBlock, "eth_getBlockByNumber", preGenesis, false)
	hits, requests, handlerErrors := sentinel.snapshot()
	t.Require().NoErrorf(err, "pre-genesis legacy routing failed: sentinel hits=%d handler errors=%v", hits, handlerErrors)
	t.Require().JSONEq("true", string(legacyBlock["legacySentinel"]), "pre-genesis block must come from the legacy endpoint")

	blockRequests := requestsForMethod(requests, "eth_getBlockByNumber")
	t.Require().Len(blockRequests, 1, "pre-genesis block request must hit the legacy endpoint exactly once")
	t.Require().Empty(handlerErrors, "legacy sentinel must decode every forwarded request")
	t.Require().Len(blockRequests[0].Params, 2)
	t.Require().JSONEq(`"`+preGenesis+`"`, string(blockRequests[0].Params[0]))

	var genesisBlock map[string]json.RawMessage
	t.Require().NoError(rpc.CallContext(t.Ctx(), &genesisBlock, "eth_getBlockByNumber", hexutil.EncodeUint64(genesisHeight), false))
	t.Require().NotEmpty(genesisBlock["hash"], "genesis block must be served locally")

	var latestBlock map[string]json.RawMessage
	t.Require().NoError(rpc.CallContext(t.Ctx(), &latestBlock, "eth_getBlockByNumber", "latest", false))
	t.Require().NotEmpty(latestBlock["hash"], "latest block must be served locally")

	_, requests, handlerErrors = sentinel.snapshot()
	blockRequests = requestsForMethod(requests, "eth_getBlockByNumber")
	t.Require().Len(blockRequests, 1, "genesis and latest requests must not hit the legacy endpoint")
	t.Require().Empty(handlerErrors, "legacy sentinel must not encounter handler errors")
}
