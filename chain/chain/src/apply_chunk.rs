use crate::chain::BlockContext;
use near_primitives::receipt::Receipt;
use near_primitives::shard_layout::ShardUId;
use near_primitives::sharding::{ShardChunk, ShardChunkHeader};
use near_primitives::types::chunk_extra::ChunkExtra;
use near_primitives::types::ShardId;
use std::sync::Arc;

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

pub enum ApplyChunkType {
    YesNew(ShardChunk),
    YesOld(Arc<ChunkExtra>),
    MaybeSplit,
}

pub struct ShardApplyInfo {
    pub shard_uid: ShardUId,
    pub cares_about_shard_this_epoch: bool,
    pub cares_about_shard_next_epoch: bool,
    pub will_shard_layout_change: bool,
}
