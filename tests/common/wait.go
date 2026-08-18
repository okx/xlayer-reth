package common

import (
	"context"
	"errors"
	"fmt"
	"math/big"
	"time"

	"github.com/ethereum/go-ethereum/common"
	"github.com/ethereum/go-ethereum/core/types"
)

// pollInterval is the delay between polls while waiting on chain progress.
const pollInterval = 1 * time.Second

// WaitForBlocks blocks until the client reports at least minBlocks height, or the
// context expires. It ports the Rust wait_for_blocks helper.
func (c *DevnetClient) WaitForBlocks(ctx context.Context, minBlocks uint64) (uint64, error) {
	ticker := time.NewTicker(pollInterval)
	defer ticker.Stop()
	for {
		n, err := c.BlockNumber(ctx)
		if err == nil && n >= minBlocks {
			return n, nil
		}
		select {
		case <-ctx.Done():
			return 0, fmt.Errorf("waiting for %d blocks: %w", minBlocks, ctx.Err())
		case <-ticker.C:
		}
	}
}

// WaitForBlockNumber blocks until the chain head reaches target height.
func (c *DevnetClient) WaitForBlockNumber(ctx context.Context, target uint64) error {
	_, err := c.WaitForBlocks(ctx, target)
	return err
}

// WaitForTxMined polls the receipt for hash until it is mined, then requires a
// successful status. It ports the Rust wait_for_tx_mined helper.
func (c *DevnetClient) WaitForTxMined(ctx context.Context, hash common.Hash) (*types.Receipt, error) {
	ticker := time.NewTicker(pollInterval)
	defer ticker.Stop()
	for {
		receipt, err := c.TransactionReceipt(ctx, hash)
		if err == nil && receipt != nil {
			if receipt.Status != types.ReceiptStatusSuccessful {
				return receipt, fmt.Errorf("tx %s mined with failed status %d", hash.Hex(), receipt.Status)
			}
			return receipt, nil
		}
		select {
		case <-ctx.Done():
			return nil, fmt.Errorf("waiting for tx %s: %w", hash.Hex(), ctx.Err())
		case <-ticker.C:
		}
	}
}

// BlockIdentity is the (stateRoot, hash) pair used to assert two nodes agree on a
// canonical block.
type BlockIdentity struct {
	StateRoot common.Hash
	Hash      common.Hash
}

// BlockIdentityAt returns the state root and hash of the block at number.
func (c *DevnetClient) BlockIdentityAt(ctx context.Context, number uint64) (BlockIdentity, error) {
	block, err := c.BlockByNumber(ctx, new(big.Int).SetUint64(number))
	if err != nil {
		return BlockIdentity{}, err
	}
	return BlockIdentity{StateRoot: block.Root(), Hash: block.Hash()}, nil
}

// AssertNodesAgree waits for both nodes to reach the given height and returns an
// error unless they report an identical state root and block hash. It ports the
// Rust assert_nodes_agree_on_block check used by the gasless consensus scenario.
func (c *DevnetClient) AssertNodesAgree(ctx context.Context, other *DevnetClient, number uint64) error {
	if err := c.WaitForBlockNumber(ctx, number); err != nil {
		return err
	}
	if err := other.WaitForBlockNumber(ctx, number); err != nil {
		return err
	}
	a, err := c.BlockIdentityAt(ctx, number)
	if err != nil {
		return err
	}
	b, err := other.BlockIdentityAt(ctx, number)
	if err != nil {
		return err
	}
	if a != b {
		return errors.New("nodes disagree on block " + fmt.Sprint(number) +
			": " + a.Hash.Hex() + "/" + a.StateRoot.Hex() +
			" vs " + b.Hash.Hex() + "/" + b.StateRoot.Hex())
	}
	return nil
}
