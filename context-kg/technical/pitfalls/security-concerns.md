# Pitfalls: Security Concerns

## 1. JSON Injection in Legacy RPC Routing

**Risk**: Block hash parameters are interpolated into JSON-RPC request strings. Malicious hash values containing quotes or special characters could inject arbitrary JSON.

**Mitigation**: `is_valid_32_bytes_string()` validates:
- Starts with `0x`
- Exactly 66 characters
- All characters after `0x` are valid ASCII hex digits

This is called in both `parse_block_param()` and `call_eth_get_block_by_hash()` before any string interpolation.

**Affected code**: `crates/legacy-rpc/src/lib.rs`, `crates/legacy-rpc/src/service.rs`

## 2. Bridge Intercept Address Validation

**Risk**: Invalid Ethereum addresses in bridge intercept configuration could cause panics via `.expect("validated")` in `to_bridge_intercept_config()`.

**Mitigation**:
- `XLayerInterceptArgs::validate()` checks address parseability BEFORE `to_bridge_intercept_config()` is called
- When `enabled: false`, address parsing is completely skipped — the function returns `BridgeInterceptConfig::default()` immediately
- Test `test_xlayer_intercept_disabled_with_invalid_addresses_does_not_panic` verifies this path

## 3. Builder Private Key Handling

**Risk**: The builder's signing key is passed via `--xlayer.builder.secret-key` CLI argument.

**Mitigation**:
- Key is only used in sequencer mode for signing builder transactions
- `Signer` struct in `crates/builder/src/signer.rs` handles key material
- No key logging or serialization outside of signing operations
- For testing, `Signer::random()` generates ephemeral keys

## 4. WebSocket Publisher Exposure

**Risk**: The WebSocket publisher listens on a configurable address/port and broadcasts flashblock data to any connected client.

**Mitigation**:
- Configurable subscriber limit prevents resource exhaustion
- Default listen address should be localhost/internal only
- No authentication mechanism — relies on network-level access control

## 5. Legacy RPC Endpoint Trust

**Risk**: The legacy RPC endpoint is an external HTTP service. Responses are parsed and forwarded to users.

**Mitigation**:
- Configurable timeout prevents indefinite blocking
- Response parsing uses standard JSON deserialization (no eval/exec)
- Error responses from legacy are translated to standard JSON-RPC errors
- The endpoint URL is validated at startup via `Url::parse()`

## 6. Transaction Hash Extraction in Monitor

**Risk**: The RPC monitor extracts transaction hashes from response JSON. A malformed response could cause parsing failures.

**Mitigation**: The extraction uses optional chaining (`and_then`) — parsing failures result in `None` and the transaction is simply not monitored. No panics or error propagation.

## 7. Environment Variable for Genesis Override

**Risk**: `XLayerChainSpecParser` supports `legacyXLayerBlock` override via environment variable, which could alter chain behavior.

**Mitigation**: This is a deployment configuration mechanism, not a runtime input. The environment variable is only read during chain spec parsing at startup.
