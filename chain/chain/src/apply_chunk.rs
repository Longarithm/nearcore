use crate::chain::BlockContext;
use near_primitives::receipt::Receipt;
use near_primitives::sharding::{ShardChunk, ShardChunkHeader};
use near_primitives::types::chunk_extra::ChunkExtra;
use near_primitives::types::ShardId;

/// apply_chunks may be called in two code paths, through process_block or through catchup_blocks
/// When it is called through process_block, it is possible that the shard state for the next epoch
/// has not been caught up yet, thus the two modes IsCaughtUp and NotCaughtUp.
/// CatchingUp is for when apply_chunks is called through catchup_blocks, this is to catch up the
/// shard states for the next epoch
#[derive(Eq, PartialEq, Copy, Clone, Debug)]
pub enum ApplyChunksMode {
    IsCaughtUp,
    CatchingUp,
    NotCaughtUp,
}

pub enum ShouldApplyTransactions {
    Yes(ShardChunk, ShardChunk),
    No(ShardChunkHeader, ShardChunkHeader),
}

pub enum NewMode {
    Classic,
    Shadow(Vec<BlockContext>, ChunkExtra, Vec<Receipt>),
}
