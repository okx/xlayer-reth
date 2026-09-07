package rcs_filter

import (
	"encoding/json"
	"fmt"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"sync"
	"sync/atomic"
)

// testingT is the minimal test-handle surface the mock needs. Both the standard-library
// *testing.T (used by the mock's own unit test) and op-devstack's devtest.T (used by the
// op-devstack E2E scenario) satisfy it, so one StartMockRCS serves both callers.
type testingT interface {
	Helper()
	Cleanup(func())
}

// MockRCS is an in-repo, test-only stand-in for the RCS REST service. It serves the two
// rule-distribution endpoints the node polls (GET /rules, GET /rules/version) and sinks the two
// audit endpoints (POST /permission-requests/submit, GET /permission-requests/query) so a test can
// assert zero audit traffic during a deny-only window. It holds no product code.
type MockRCS struct {
	srv         *httptest.Server
	mu          sync.RWMutex
	body        []byte // current GET /rules body
	version     uint64 // current content_version
	submitCount int64
	queryCount  int64
}

// emptyRulesBody is the initial GET /rules body: a valid, empty rule set at content_version 1.
func emptyRulesBody() []byte {
	return []byte(`{"protocol_version":1,"content_version":1,"rules":[]}`)
}

// StartMockRCS starts the mock and serves an empty rule set at content_version 1.
func StartMockRCS(t testingT) *MockRCS {
	t.Helper()
	m := &MockRCS{body: emptyRulesBody(), version: 1}
	mux := http.NewServeMux()
	mux.HandleFunc("/rules/version", func(w http.ResponseWriter, _ *http.Request) {
		m.mu.RLock()
		v := m.version
		m.mu.RUnlock()
		writeJSON(w, map[string]any{"protocol_version": 1, "content_version": v})
	})
	mux.HandleFunc("/rules", func(w http.ResponseWriter, _ *http.Request) {
		m.mu.RLock()
		body := m.body
		m.mu.RUnlock()
		w.Header().Set("Content-Type", "application/json")
		_, _ = w.Write(body)
	})
	mux.HandleFunc("/permission-requests/submit", func(w http.ResponseWriter, _ *http.Request) {
		atomic.AddInt64(&m.submitCount, 1)
		w.WriteHeader(http.StatusAccepted)
		_, _ = w.Write([]byte(`{}`))
	})
	mux.HandleFunc("/permission-requests/query", func(w http.ResponseWriter, _ *http.Request) {
		atomic.AddInt64(&m.queryCount, 1)
		writeJSON(w, map[string]any{})
	})
	m.srv = httptest.NewServer(mux)
	t.Cleanup(m.srv.Close)
	return m
}

func writeJSON(w http.ResponseWriter, v any) {
	w.Header().Set("Content-Type", "application/json")
	_ = json.NewEncoder(w).Encode(v)
}

// URL returns the mock base URL for --rcs-filter.rcs-base-url.
func (m *MockRCS) URL() string { return m.srv.URL }

// ActivateEmergencyRules swaps the served body to testdata/emergency_rules.json (content_version 2),
// so the node's next version probe pulls the full emergency rule set.
func (m *MockRCS) ActivateEmergencyRules() {
	body, err := os.ReadFile(filepath.Join("testdata", "emergency_rules.json"))
	if err != nil {
		panic("read testdata/emergency_rules.json: " + err.Error())
	}
	// Confirm the fixture parses and carries content_version 2 before serving it.
	var probe struct {
		ContentVersion uint64 `json:"content_version"`
	}
	if err := json.Unmarshal(body, &probe); err != nil || probe.ContentVersion == 0 {
		panic("emergency_rules.json must be valid JSON with a non-zero content_version")
	}
	m.mu.Lock()
	m.body = body
	m.version = probe.ContentVersion
	m.mu.Unlock()
}

// ContentVersion returns the currently served content_version.
func (m *MockRCS) ContentVersion() uint64 {
	m.mu.RLock()
	defer m.mu.RUnlock()
	return m.version
}

// RestoreEmptyRules reverts to an empty rule set at a higher content_version so the node hot-reloads
// back to no filtering (recovery path).
func (m *MockRCS) RestoreEmptyRules() {
	m.mu.Lock()
	m.version++
	m.body = []byte(fmt.Sprintf(`{"protocol_version":1,"content_version":%d,"rules":[]}`, m.version))
	m.mu.Unlock()
}

// SubmitCount / QueryCount return cumulative audit-endpoint hit counts.
func (m *MockRCS) SubmitCount() int { return int(atomic.LoadInt64(&m.submitCount)) }
func (m *MockRCS) QueryCount() int  { return int(atomic.LoadInt64(&m.queryCount)) }

// Close stops the server (also registered via t.Cleanup).
func (m *MockRCS) Close() { m.srv.Close() }
