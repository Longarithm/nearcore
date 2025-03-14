use std::collections::{BTreeMap, HashMap};

use near_primitives::block::Block;
use near_primitives::hash::CryptoHash;
use near_primitives::optimistic_block::CachedShardUpdateKey;
use near_primitives::types::BlockHeight;
use near_primitives::types::ShardId;

/// Blocks that are waiting for optimistic block to be applied.
pub struct PendingBlocksPool {
    blocks: HashMap<CryptoHash, Block>,
    pub height_idx: BTreeMap<BlockHeight, Vec<CryptoHash>>,
    /// Maps block hash to a map of shard_id -> cached_shard_update_key
    /// Used to track which blocks have all their shard update keys matching with optimistic blocks
    shard_update_keys: HashMap<CryptoHash, HashMap<ShardId, CachedShardUpdateKey>>,
}

impl PendingBlocksPool {
    pub fn new() -> Self {
        Self {
            blocks: HashMap::new(),
            height_idx: BTreeMap::new(),
            shard_update_keys: HashMap::new(),
        }
    }

    pub fn add_block(&mut self, block: Block) {
        self.blocks.insert(block.hash(), block);
        self.height_idx.entry(block.height()).or_insert_with(Vec::new).push(block.hash());
    }

    pub fn add_block_with_shard_keys(
        &mut self,
        block: Block,
        shard_keys: HashMap<ShardId, CachedShardUpdateKey>,
    ) {
        let block_hash = block.hash();
        self.add_block(block);
        self.shard_update_keys.insert(block_hash, shard_keys);
    }

    pub fn get_block(&self, hash: &CryptoHash) -> Option<&Block> {
        self.blocks.get(hash)
    }

    pub fn remove_block(&mut self, hash: &CryptoHash) -> Option<Block> {
        let block = self.blocks.remove(hash)?;
        let height = block.height();

        if let Some(hashes) = self.height_idx.get_mut(&height) {
            hashes.retain(|h| h != hash);
            if hashes.is_empty() {
                self.height_idx.remove(&height);
            }
        }

        self.shard_update_keys.remove(hash);

        Some(block)
    }

    pub fn get_shard_keys(
        &self,
        hash: &CryptoHash,
    ) -> Option<&HashMap<ShardId, CachedShardUpdateKey>> {
        self.shard_update_keys.get(hash)
    }

    pub fn contains(&self, hash: &CryptoHash) -> bool {
        self.blocks.contains_key(hash)
    }

    pub fn prune_blocks_below_height(&mut self, height: BlockHeight) {
        let heights_to_remove: Vec<BlockHeight> =
            self.height_idx.keys().copied().take_while(|h| *h < height).collect();
        for h in heights_to_remove {
            let Some(block_hashes) = self.height_idx.remove(&h) else {
                continue;
            };

            for block_hash in block_hashes {
                self.blocks.remove(&block_hash);
                self.shard_update_keys.remove(&block_hash);
            }
        }
    }
}
