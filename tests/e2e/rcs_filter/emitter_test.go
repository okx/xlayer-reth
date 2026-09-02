package rcs_filter

import (
	"testing"

	"github.com/ethereum/go-ethereum/crypto"
)

// TestEmitterTransferTopic0Matches pins the transferTopic0 constant to the live keccak of the
// canonical ERC-20 Transfer signature, so the emitter's LOG3 topic0 can never silently drift from
// the event the emergency rule fixture declares. It also guards that the creation bytecode is
// present and its documented byte length is unchanged.
func TestEmitterTransferTopic0Matches(t *testing.T) {
	want := crypto.Keccak256Hash([]byte("Transfer(address,address,uint256)"))
	if transferTopic0 != want {
		t.Fatalf("transferTopic0 = %s, want %s", transferTopic0, want)
	}
	// 12-byte constructor + 53-byte runtime = 65 bytes (see emitter.go byte layout).
	if got := len(emitterCreationBytecode); got != 65 {
		t.Fatalf("emitterCreationBytecode length = %d bytes, want 65", got)
	}
}
