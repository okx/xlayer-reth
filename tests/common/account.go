package common

import (
	"context"
	"crypto/ecdsa"
	"fmt"
	"math/big"

	"github.com/ethereum/go-ethereum/common"
	"github.com/ethereum/go-ethereum/core/types"
	"github.com/ethereum/go-ethereum/crypto"
)

// Account is a transaction sender bound to one chain id. Its private key is
// supplied by the devnet's funder at runtime, so no secret is ever hard-coded.
type Account struct {
	priv    *ecdsa.PrivateKey
	chainID *big.Int
	signer  types.Signer
	// Address is the sender's EOA address.
	Address common.Address
}

// NewAccount builds a sender from a runtime-provided private key and chain id.
func NewAccount(priv *ecdsa.PrivateKey, chainID *big.Int) *Account {
	return &Account{
		priv:    priv,
		chainID: chainID,
		signer:  types.LatestSignerForChainID(chainID),
		Address: crypto.PubkeyToAddress(priv.PublicKey),
	}
}

// dynamicFeeParams bundles the EIP-1559 fee inputs for one transaction.
type dynamicFeeParams struct {
	to        *common.Address
	value     *big.Int
	data      []byte
	gasLimit  uint64
	gasTip    *big.Int
	gasFeeCap *big.Int
}

// send signs and submits an EIP-1559 transaction with an auto-fetched nonce and
// returns the resulting transaction hash.
func (a *Account) send(ctx context.Context, c *DevnetClient, p dynamicFeeParams) (common.Hash, error) {
	nonce, err := c.NonceAt(ctx, a.Address)
	if err != nil {
		return common.Hash{}, fmt.Errorf("nonce: %w", err)
	}
	value := p.value
	if value == nil {
		value = big.NewInt(0)
	}
	tip := p.gasTip
	if tip == nil {
		tip = big.NewInt(0)
	}
	feeCap := p.gasFeeCap
	if feeCap == nil {
		feeCap = big.NewInt(0)
	}
	tx := types.NewTx(&types.DynamicFeeTx{
		ChainID:   a.chainID,
		Nonce:     nonce,
		GasTipCap: tip,
		GasFeeCap: feeCap,
		Gas:       p.gasLimit,
		To:        p.to,
		Value:     value,
		Data:      p.data,
	})
	signed, err := types.SignTx(tx, a.signer, a.priv)
	if err != nil {
		return common.Hash{}, fmt.Errorf("sign: %w", err)
	}
	if err := c.SendTransaction(ctx, signed); err != nil {
		return common.Hash{}, fmt.Errorf("send: %w", err)
	}
	return signed.Hash(), nil
}

// TransferValue sends a native transfer priced at the given fee caps.
func (a *Account) TransferValue(ctx context.Context, c *DevnetClient, to common.Address, value, gasTip, gasFeeCap *big.Int) (common.Hash, error) {
	return a.send(ctx, c, dynamicFeeParams{to: &to, value: value, gasLimit: 21_000, gasTip: gasTip, gasFeeCap: gasFeeCap})
}

// CallContract sends a contract call priced at the given fee caps.
func (a *Account) CallContract(ctx context.Context, c *DevnetClient, to common.Address, data []byte, gasLimit uint64, gasTip, gasFeeCap *big.Int) (common.Hash, error) {
	return a.send(ctx, c, dynamicFeeParams{to: &to, data: data, gasLimit: gasLimit, gasTip: gasTip, gasFeeCap: gasFeeCap})
}

// SendGaslessTransfer sends a zero-priced transfer (both fee caps zero) with the
// gasless probe calldata, so the execution client takes the gasless path.
func (a *Account) SendGaslessTransfer(ctx context.Context, c *DevnetClient, to common.Address, value *big.Int) (common.Hash, error) {
	return a.send(ctx, c, dynamicFeeParams{
		to:        &to,
		value:     value,
		data:      GaslessProbeInput,
		gasLimit:  DefaultGaslessGasLimit,
		gasTip:    big.NewInt(0),
		gasFeeCap: big.NewInt(0),
	})
}

// Deploy submits a contract-creation transaction with the given init code and
// returns the transaction hash and the deterministic deployed address.
func (a *Account) Deploy(ctx context.Context, c *DevnetClient, initCode []byte, gasLimit uint64, gasFeeCap *big.Int) (common.Hash, common.Address, error) {
	nonce, err := c.NonceAt(ctx, a.Address)
	if err != nil {
		return common.Hash{}, common.Address{}, fmt.Errorf("nonce: %w", err)
	}
	addr := crypto.CreateAddress(a.Address, nonce)
	h, err := a.send(ctx, c, dynamicFeeParams{data: initCode, gasLimit: gasLimit, gasFeeCap: gasFeeCap})
	if err != nil {
		return common.Hash{}, common.Address{}, err
	}
	return h, addr, nil
}
