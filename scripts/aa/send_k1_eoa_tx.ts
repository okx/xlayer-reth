/**
 * send_k1_eoa_tx.ts — submit a minimal K1-native (EOA) EIP-8130 tx.
 *
 * Builds, signs, and submits a single-call EIP-8130 (0x7B) transaction
 * against a running `xlayer-reth-node --chain xlayer-dev --dev` instance.
 *
 * Wire layout (see `op_alloy_consensus::eip8130`):
 *
 *   preimage = 0x7B || rlp([chain_id, from, nonce_key, nonce_sequence, expiry,
 *                           gas_price, gas_limit,
 *                           account_changes, calls, payer])
 *   tx       = 0x7B || rlp([...all above..., sender_auth, payer_auth])
 *
 *   • `from` / `payer` are `Option<Address>`: empty RLP string when None,
 *     20-byte RLP string when Some.
 *   • `calls` is `Vec<Vec<Call>>`; each inner Call is `[to, data]`.
 *   • `sender_auth` for K1 EOA mode = 65-byte `r || s || (27+recid)` ECDSA
 *     signature over keccak256(preimage).
 *   • Fee model is legacy-style: a single `gas_price` field — XLayer does
 *     not run EIP-1559 dynamic base-fee accounting for AA txs, so there is
 *     no `max_priority_fee_per_gas` on the wire.
 *
 * See `../README.md` for env-var options.
 */

import {
    createPublicClient,
    hexToBytes,
    http,
    keccak256,
    toBytes,
    toHex,
    toRlp,
    TransactionRejectedRpcError,
    type Hex,
} from "viem";
import { secp256k1 } from "@noble/curves/secp256k1";

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

const RPC = process.env.RPC ?? "http://127.0.0.1:8545";
// xlayer-dev's pre-funded rich key — see crates/chainspec/src/xlayer_dev.rs.
const PRIV = (process.env.PRIV ??
    "0x4bbbf85ce3377467afe5d46f804f221813b2bb87f24d81f60f1fcdbf7cbf4356") as Hex;
const TO = (process.env.TO ??
    "0x0000000000000000000000000000000000000001") as Hex;
const NONCE_KEY = BigInt(process.env.NONCE_KEY ?? "0");
const GAS_LIMIT = BigInt(process.env.GAS_LIMIT ?? "200000");
// XLayer AA uses a single legacy-style `gas_price` field. `MAX_FEE` is kept
// as an alias env var for ergonomics (matches the struct name historically
// used while we were tracking Base's spec).
const GAS_PRICE = BigInt(
    process.env.GAS_PRICE ?? process.env.MAX_FEE ?? "1000000000",
);
const EXPIRY = BigInt(process.env.EXPIRY ?? "0");

const AA_TX_TYPE = 0x7b;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/** Big-endian bytes with leading zeros stripped — matches alloy_rlp's uint encoding. */
function bigintToMinimalBytes(x: bigint): Uint8Array {
    if (x === 0n) return new Uint8Array(0);
    let hex = x.toString(16);
    if (hex.length % 2 === 1) hex = "0" + hex;
    return hexToBytes(`0x${hex}`);
}

function optionalAddress(addr: Hex | null): Uint8Array {
    return addr === null ? new Uint8Array(0) : hexToBytes(addr);
}

/** Derive an Ethereum address from an secp256k1 private key. */
function addressFromPriv(priv: Hex): Hex {
    const pub = secp256k1.getPublicKey(hexToBytes(priv), false); // 65 bytes, 0x04-prefixed
    const hash = keccak256(pub.slice(1));
    return `0x${hash.slice(-40)}` as Hex;
}

// viem's `toRlp` accepts `Hex | Hex[]` (recursive). Our fields are a mix of
// bigints / bytes / nested lists — convert everything to `Hex` at the leaves.
type RlpLeaf = Hex;
type RlpNode = RlpLeaf | RlpNode[];

function bytesToRlpHex(b: Uint8Array): Hex {
    return toHex(b);
}

function uintToRlpHex(x: bigint): Hex {
    return bytesToRlpHex(bigintToMinimalBytes(x));
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

async function main() {
    const client = createPublicClient({ transport: http(RPC) });

    const chainIdHex = await client.request({ method: "eth_chainId" });
    const chainId = BigInt(chainIdHex);

    const sender = addressFromPriv(PRIV);

    // eth_getTransactionCount at this sender returns the standard nonce which
    // is fine for the default nonce_key=0 channel (the NonceManager stores the
    // 2D-nonce sequence in the same slot the standard EOA nonce tracker uses
    // when nonce_key=0). If you override NONCE_KEY, pass NONCE_SEQ explicitly.
    const nonceSeq = process.env.NONCE_SEQ
        ? BigInt(process.env.NONCE_SEQ)
        : BigInt(
            await client.request({
                method: "eth_getTransactionCount",
                params: [sender, "pending"],
            }),
        );

    const balance = BigInt(
        await client.request({
            method: "eth_getBalance",
            params: [sender, "latest"],
        }),
    );

    console.log(`RPC        : ${RPC}`);
    console.log(`chain_id   : ${chainId}`);
    console.log(`sender     : ${sender}`);
    console.log(`balance    : ${balance} wei`);
    console.log(`nonce_key  : ${NONCE_KEY}`);
    console.log(`nonce_seq  : ${nonceSeq}`);
    console.log(`to         : ${TO}`);
    console.log(`gas_limit  : ${GAS_LIMIT}`);
    console.log(`gas_price  : ${GAS_PRICE}`);
    console.log(`expiry     : ${EXPIRY}`);
    if (balance === 0n) {
        throw new Error(
            `sender has zero balance; set PRIV= to a pre-funded key or fund ${sender}`,
        );
    }

    // Build RLP-shaped fields. Order + shape must match the Rust
    // `TxEip8130::encode_fields` exactly. Legacy-style fee: single
    // `gas_price` between `expiry` and `gas_limit`.
    const preimageFields: RlpNode = [
        uintToRlpHex(chainId),
        bytesToRlpHex(optionalAddress(null)), // from = None
        uintToRlpHex(NONCE_KEY),
        uintToRlpHex(nonceSeq),
        uintToRlpHex(EXPIRY),
        uintToRlpHex(GAS_PRICE),
        uintToRlpHex(GAS_LIMIT),
        [], // account_changes: empty list
        // calls: Vec<Vec<Call>> ; one phase with one call (to, data=empty)
        [[[TO, "0x"]]],
        bytesToRlpHex(optionalAddress(null)), // payer = None
    ];
    const preimagePayload = toRlp(preimageFields, "bytes");
    // 0x7B || rlp([...])
    const preimage = new Uint8Array(1 + preimagePayload.length);
    preimage[0] = AA_TX_TYPE;
    preimage.set(preimagePayload, 1);

    const prehash = keccak256(preimage, "bytes");

    // Sign the prehash. `@noble/curves` returns a compact 64-byte (r||s)
    // sig plus a recovery id. We pack to 65 bytes: r||s||(27+recid) to
    // match EIP-8130's ecrecover-style `sender_auth` convention.
    const sig = secp256k1.sign(prehash, hexToBytes(PRIV), { lowS: true });
    const senderAuth = new Uint8Array(65);
    senderAuth.set(sig.toCompactRawBytes(), 0);
    senderAuth[64] = 27 + (sig.recovery ?? 0);

    const txFields: RlpNode = [
        ...(preimageFields as RlpNode[]),
        bytesToRlpHex(senderAuth),
        "0x", // payer_auth = empty
    ];
    const txPayload = toRlp(txFields, "bytes");
    const raw = new Uint8Array(1 + txPayload.length);
    raw[0] = AA_TX_TYPE;
    raw.set(txPayload, 1);
    const rawHex = toHex(raw);

    console.log(`\nraw tx len : ${raw.length} bytes`);
    console.log(`raw tx     : ${rawHex}`);

    // The tx's sender_auth (K1 ECDSA) is deterministic in the current k256
    // crate — same inputs → same signature → same tx hash. Re-running this
    // script against the same pre-mined pool produces "already known",
    // which is a success signal: it means the tx is still admitted.
    let txHash: Hex;
    try {
        txHash = (await client.request({
            method: "eth_sendRawTransaction",
            params: [rawHex],
        })) as Hex;
    } catch (err) {
        const detail =
            err instanceof TransactionRejectedRpcError ? err.details : String(err);
        if (typeof detail === "string" && detail.toLowerCase().includes("already known")) {
            txHash = toHex(keccak256(raw, "bytes"));
            console.log(`\n(tx already in pool; using derived hash)`);
        } else {
            throw err;
        }
    }
    console.log(`\ntx hash    : ${txHash}`);

    // Verify the full in-pool → mined round trip. The payload builder
    // now projects `OpTxEnvelope::Eip8130` into `XLayerAAParts` via
    // `parts_from_eip8130` (see crates/xlayer-revm/src/tx_env.rs), so
    // `validate_against_state_and_deduct_caller` loads fees from the
    // right account and the AA handler runs end-to-end.
    console.log(`\n==> waiting for receipt (up to 30s)`);
    const deadline = Date.now() + 30_000;
    type ReceiptShape = {
        status: Hex;
        blockNumber: Hex;
        transactionIndex: Hex;
        gasUsed: Hex;
        from: Hex;
        type: Hex;
    } | null;
    let receipt: ReceiptShape = null;
    while (Date.now() < deadline) {
        receipt = (await client.request({
            method: "eth_getTransactionReceipt",
            params: [txHash],
        })) as ReceiptShape;
        if (receipt !== null) break;
        await new Promise((r) => setTimeout(r, 500));
    }
    if (receipt === null) {
        console.error("!! receipt did not land within 30s");
        process.exit(2);
    }
    console.log(JSON.stringify(receipt, null, 2));
    if (BigInt(receipt.type) !== BigInt(AA_TX_TYPE)) {
        console.error(
            `!! receipt tx-type mismatch (got ${receipt.type}, want ${toHex(AA_TX_TYPE)})`,
        );
        process.exit(2);
    }
    if (receipt.from.toLowerCase() !== sender.toLowerCase()) {
        console.error(
            `!! receipt sender mismatch (got ${receipt.from}, expected=${sender})`,
        );
        process.exit(2);
    }
    if (receipt.status !== "0x1") {
        console.error(`!! tx reverted on-chain (status=${receipt.status})`);
        process.exit(2);
    }
    console.log(
        `\n==> AA tx mined (block=${receipt.blockNumber}, idx=${receipt.transactionIndex}, gasUsed=${receipt.gasUsed}, status=success)`,
    );
    process.exit(0);
}

main().catch((e) => {
    console.error(e);
    process.exit(1);
});
