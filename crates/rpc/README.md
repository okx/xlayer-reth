# XLayer RPC Extensions

XLayer-specific RPC methods for the Reth node.

## RPC Methods

### `eth_minGasPrice`

**Function**: Returns the minimum recommended gas price (base fee + default suggested fee)

**Parameters**: None

**Returns**: `String` - Hexadecimal string representing the minimum gas price in wei

**Request Example**:

```bash
curl -X POST http://localhost:8545 \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"eth_minGasPrice","params":[],"id":1}'
```

**Response Example**:

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "result": "0x3b9aca00"
}
```

