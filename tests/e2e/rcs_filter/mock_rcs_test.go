package rcs_filter

import (
	"encoding/json"
	"io"
	"net/http"
	"strings"
	"testing"
)

func getJSON(t *testing.T, url string) map[string]any {
	t.Helper()
	resp, err := http.Get(url)
	if err != nil {
		t.Fatalf("GET %s: %v", url, err)
	}
	defer resp.Body.Close()
	body, _ := io.ReadAll(resp.Body)
	var out map[string]any
	if err := json.Unmarshal(body, &out); err != nil {
		t.Fatalf("decode %s: %v (%s)", url, err, body)
	}
	return out
}

func TestMockRCSVersionGatedActivation(t *testing.T) {
	m := StartMockRCS(t)
	defer m.Close()

	// Initially: empty rule set at content_version 1.
	v := getJSON(t, m.URL()+"/rules/version")
	if got := v["content_version"].(float64); got != 1 {
		t.Fatalf("initial content_version = %v, want 1", got)
	}
	rules := getJSON(t, m.URL()+"/rules")["rules"].([]any)
	if len(rules) != 0 {
		t.Fatalf("initial rules = %d, want 0", len(rules))
	}

	// Activate: version bumps and the emergency rule appears.
	m.ActivateEmergencyRules()
	v = getJSON(t, m.URL()+"/rules/version")
	if got := v["content_version"].(float64); got != 2 {
		t.Fatalf("post-activation content_version = %v, want 2", got)
	}
	rules = getJSON(t, m.URL()+"/rules")["rules"].([]any)
	if len(rules) != 1 {
		t.Fatalf("post-activation rules = %d, want 1", len(rules))
	}

	// Submit/query sinks count audit traffic.
	if _, err := http.Post(m.URL()+"/permission-requests/submit", "application/json", strings.NewReader("{}")); err != nil {
		t.Fatalf("submit: %v", err)
	}
	if m.SubmitCount() != 1 {
		t.Fatalf("SubmitCount = %d, want 1", m.SubmitCount())
	}
}

func TestMockRCSRestoreEmptyRules(t *testing.T) {
	m := StartMockRCS(t)
	defer m.Close()

	m.ActivateEmergencyRules()
	if got := len(getJSON(t, m.URL()+"/rules")["rules"].([]any)); got != 1 {
		t.Fatalf("post-activation rules = %d, want 1", got)
	}
	activatedVersion := m.ContentVersion()

	// Recovery: revert to an empty rule set at a higher content_version so the node hot-reloads.
	m.RestoreEmptyRules()
	if got := m.ContentVersion(); got <= activatedVersion {
		t.Fatalf("restored content_version = %d, want > %d", got, activatedVersion)
	}
	v := getJSON(t, m.URL()+"/rules/version")
	if got := v["content_version"].(float64); uint64(got) != m.ContentVersion() {
		t.Fatalf("served content_version = %v, want %d", got, m.ContentVersion())
	}
	if got := len(getJSON(t, m.URL()+"/rules")["rules"].([]any)); got != 0 {
		t.Fatalf("post-recovery rules = %d, want 0", got)
	}
}
