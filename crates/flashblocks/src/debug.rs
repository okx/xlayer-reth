use crate::cache::FlashblockStateCache;
use reth_chain_state::ExecutedBlock;
use reth_primitives_traits::NodePrimitives;
use tracing::{debug, info, warn};

/// Captures the flashblocks and engine `ExecutedBlock`s synchronously (cheap Arc clones),
/// then spawns the heavy comparison on a blocking thread to avoid stalling the canonical
/// stream handler.
pub(crate) fn debug_compare_flashblocks_bundle_states<N: NodePrimitives + 'static>(
    flashblocks_state: &FlashblockStateCache<N>,
    block_number: u64,
    block_hash: alloy_primitives::B256,
) {
    // Capture data synchronously (before handle_canonical_block evicts the cache).
    // These are cheap — ExecutedBlock internals are Arc'd.
    let fb_block = flashblocks_state.debug_get_executed_block_by_number(block_number);
    let engine_block = flashblocks_state
        .canon_in_memory_state
        .state_by_hash(block_hash)
        .map(|state| state.block());

    // Spawn the heavy comparison on a blocking thread so the canonical stream handler
    // stays responsive. trie_data() is synchronous (parking_lot::Mutex, no async).
    tokio::task::spawn_blocking(move || {
        compare_executed_blocks(fb_block, engine_block, block_number);
    });
}

/// Performs the deep comparison between flashblocks and engine `ExecutedBlock`s.
fn compare_executed_blocks<N: NodePrimitives>(
    fb_block: Option<ExecutedBlock<N>>,
    engine_block: Option<ExecutedBlock<N>>,
    block_number: u64,
) {
    let (Some(fb), Some(eng)) = (fb_block, engine_block) else {
        debug!(
            target: "flashblocks::verify",
            block_number,
            "Skipping BundleState comparison (block not available in both caches)"
        );
        return;
    };

    let fb_hash = fb.recovered_block.hash();
    let eng_hash = eng.recovered_block.hash();

    let fb_bundle = &fb.execution_output.state;
    let eng_bundle = &eng.execution_output.state;

    // Deep compare accounts: match by address, compare BundleAccount fields
    let mut account_mismatches = Vec::new();
    let mut fb_only = Vec::new();
    let mut eng_only = Vec::new();
    for (addr, fb_acct) in &fb_bundle.state {
        if let Some(eng_acct) = eng_bundle.state.get(addr) {
            if fb_acct != eng_acct {
                account_mismatches.push(*addr);
            }
        } else {
            fb_only.push(*addr);
        }
    }
    for addr in eng_bundle.state.keys() {
        if !fb_bundle.state.contains_key(addr) {
            eng_only.push(*addr);
        }
    }

    // Deep compare reverts: both should have exactly 1 entry after flattening.
    // Match by address within each revert vec.
    let mut revert_mismatches = Vec::new();
    let mut revert_fb_only = Vec::new();
    let mut revert_eng_only = Vec::new();
    if fb_bundle.reverts.len() == eng_bundle.reverts.len() {
        for (fb_rev, eng_rev) in fb_bundle.reverts.iter().zip(eng_bundle.reverts.iter()) {
            let fb_map: std::collections::HashMap<_, _> =
                fb_rev.iter().map(|(a, r)| (a, r)).collect();
            let eng_map: std::collections::HashMap<_, _> =
                eng_rev.iter().map(|(a, r)| (a, r)).collect();
            for (addr, fb_r) in &fb_map {
                if let Some(eng_r) = eng_map.get(addr) {
                    if fb_r != eng_r {
                        revert_mismatches.push(**addr);
                    }
                } else {
                    revert_fb_only.push(**addr);
                }
            }
            for addr in eng_map.keys() {
                if !fb_map.contains_key(addr) {
                    revert_eng_only.push(**addr);
                }
            }
        }
    }

    // Compare hashed_state (the state diff input to trie computation).
    // This confirms the incremental BundleState produces the same hashed diff
    // as a fresh execution — critical since we send hashed_state to the engine pre-warm.
    let fb_trie = fb.trie_data();
    let eng_trie = eng.trie_data();
    let hashed_state_match = *fb_trie.hashed_state == *eng_trie.hashed_state;
    let trie_updates_match = *fb_trie.trie_updates == *eng_trie.trie_updates;

    let all_match = fb_hash == eng_hash
        && account_mismatches.is_empty()
        && fb_only.is_empty()
        && eng_only.is_empty()
        && fb_bundle.reverts.len() == eng_bundle.reverts.len()
        && revert_mismatches.is_empty()
        && revert_fb_only.is_empty()
        && revert_eng_only.is_empty()
        && hashed_state_match
        && trie_updates_match;

    if all_match {
        info!(
            target: "flashblocks::verify",
            block_number,
            %fb_hash,
            accounts = fb_bundle.state.len(),
            reverts = fb_bundle.reverts.len(),
            "Execution output MATCH: flashblocks == engine"
        );
    } else {
        warn!(
            target: "flashblocks::verify",
            block_number,
            fb_hash = %fb_hash,
            eng_hash = %eng_hash,
            hash_match = fb_hash == eng_hash,
            fb_accounts = fb_bundle.state.len(),
            eng_accounts = eng_bundle.state.len(),
            account_mismatches = account_mismatches.len(),
            fb_only_accounts = fb_only.len(),
            eng_only_accounts = eng_only.len(),
            fb_reverts = fb_bundle.reverts.len(),
            eng_reverts = eng_bundle.reverts.len(),
            revert_mismatches = revert_mismatches.len(),
            revert_fb_only = revert_fb_only.len(),
            revert_eng_only = revert_eng_only.len(),
            hashed_state_match,
            trie_updates_match,
            "Execution output MISMATCH: flashblocks != engine"
        );
        for addr in account_mismatches.iter().take(3) {
            warn!(target: "flashblocks::verify", %addr, "Account state mismatch");
        }
        for addr in revert_mismatches.iter().take(3) {
            warn!(target: "flashblocks::verify", %addr, "Revert mismatch");
        }
    }
}
