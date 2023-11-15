use crate::crypto_hash_timer::CryptoHashTimer;
use crate::types::{
    ApplySplitStateResult, ApplySplitStateResultOrStateChanges, ApplyTransactionResult,
    RuntimeAdapter, RuntimeStorageConfig,
};
use near_chain_primitives::Error;
use near_epoch_manager::EpochManagerAdapter;
use near_primitives::challenge::ChallengesResult;
use near_primitives::hash::CryptoHash;
use near_primitives::receipt::Receipt;
use near_primitives::sandbox::state_patch::SandboxStatePatch;
use near_primitives::shard_layout::ShardUId;
use near_primitives::sharding::ShardChunk;
use near_primitives::types::chunk_extra::ChunkExtra;
use near_primitives::types::{
    Balance, BlockHeight, EpochId, Gas, StateChangesForSplitStates, StateRoot,
};
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Clone, Debug)]
pub struct BlockContext {
    pub block_hash: CryptoHash,
    pub prev_block_hash: CryptoHash,
    pub challenges_result: ChallengesResult,
    pub block_timestamp: u64,
    pub gas_price: Balance,
    pub height: BlockHeight,
    pub random_seed: CryptoHash,
    pub is_first_block_with_chunk_of_version: bool,
    pub next_epoch_id: EpochId,
}

#[derive(Debug)]
pub struct SameHeightResult {
    pub(crate) shard_uid: ShardUId,
    pub(crate) gas_limit: Gas,
    pub(crate) apply_result: ApplyTransactionResult,
    pub(crate) apply_split_result_or_state_changes: Option<ApplySplitStateResultOrStateChanges>,
}

#[derive(Debug)]
pub struct DifferentHeightResult {
    pub(crate) shard_uid: ShardUId,
    pub(crate) apply_result: ApplyTransactionResult,
    pub(crate) apply_split_result_or_state_changes: Option<ApplySplitStateResultOrStateChanges>,
}

#[derive(Debug)]
pub struct SplitStateResult {
    // parent shard of the split states
    pub(crate) shard_uid: ShardUId,
    pub(crate) results: Vec<ApplySplitStateResult>,
}

#[derive(Debug)]
pub enum ApplyChunkResult {
    SameHeight(SameHeightResult),
    DifferentHeight(DifferentHeightResult),
    SplitState(SplitStateResult),
}

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

pub enum ShardUpdateType {
    NewChunk(ShardChunk),
    OldChunk(ChunkExtra),
    StateSplit(StateChangesForSplitStates),
}

pub struct FullShardApplyInfo {
    pub shard_uid: ShardUId,
    pub cares_about_shard_this_epoch: bool,
    pub cares_about_shard_next_epoch: bool,
    pub will_shard_layout_change: bool,
}

pub struct ShardInfo {
    pub shard_uid: ShardUId,
    pub will_shard_layout_change: bool,
}

/// This method returns the closure that is responsible for applying of a single chunk.
pub fn apply_chunk(
    parent_span: &tracing::Span,
    epoch_manager: Arc<dyn EpochManagerAdapter>,
    runtime: Arc<dyn RuntimeAdapter>,
    block_context: BlockContext,
    apply_chunk_type: ShardUpdateType,
    shard_info: ShardInfo,
    state_patch: SandboxStatePatch,
    split_state_roots: Option<HashMap<ShardUId, StateRoot>>,
    receipts: Vec<Receipt>,
) -> Result<ApplyChunkResult, Error> {
    match apply_chunk_type {
        ShardUpdateType::NewChunk(chunk) => apply_new_chunk(
            parent_span,
            block_context,
            chunk,
            shard_info,
            receipts,
            state_patch,
            runtime.clone(),
            epoch_manager.clone(),
            split_state_roots,
        ),
        ShardUpdateType::OldChunk(prev_chunk_extra) => apply_old_chunk(
            parent_span,
            block_context,
            &prev_chunk_extra,
            shard_info,
            state_patch,
            runtime.clone(),
            epoch_manager.clone(),
            split_state_roots,
        ),
        ShardUpdateType::StateSplit(state_changes) => apply_split_state_chunk(
            parent_span,
            block_context,
            shard_info.shard_uid,
            runtime,
            epoch_manager,
            split_state_roots.unwrap(),
            state_changes,
        ),
    }
}

/// Returns the apply chunk job when applying a new chunk and applying transactions.
fn apply_new_chunk(
    parent_span: &tracing::Span,
    block_context: BlockContext,
    chunk: ShardChunk,
    shard_info: ShardInfo,
    receipts: Vec<Receipt>,
    state_patch: SandboxStatePatch,
    runtime: Arc<dyn RuntimeAdapter>,
    epoch_manager: Arc<dyn EpochManagerAdapter>,
    split_state_roots: Option<HashMap<ShardUId, CryptoHash>>,
) -> Result<ApplyChunkResult, Error> {
    let shard_id = shard_info.shard_uid.shard_id();
    let _span = tracing::debug_span!(
            target: "chain",
            parent: parent_span,
            "new_chunk",
            shard_id)
    .entered();
    let chunk_inner = chunk.cloned_header().take_inner();
    let gas_limit = chunk_inner.gas_limit();

    let _timer = CryptoHashTimer::new(chunk.chunk_hash().0);
    let storage_config = RuntimeStorageConfig {
        state_root: *chunk_inner.prev_state_root(),
        use_flat_storage: true,
        source: crate::types::StorageDataSource::Db,
        state_patch,
        record_storage: false,
    };
    match runtime.apply_transactions(
        shard_id,
        storage_config,
        block_context.height,
        block_context.block_timestamp,
        &block_context.prev_block_hash,
        &block_context.block_hash,
        &receipts,
        chunk.transactions(),
        chunk_inner.prev_validator_proposals(),
        block_context.gas_price,
        gas_limit,
        &block_context.challenges_result,
        block_context.random_seed,
        true,
        block_context.is_first_block_with_chunk_of_version,
    ) {
        Ok(apply_result) => {
            let apply_split_result_or_state_changes = if shard_info.will_shard_layout_change {
                Some(apply_split_state_changes(
                    epoch_manager.as_ref(),
                    runtime.as_ref(),
                    &block_context.block_hash,
                    &block_context.prev_block_hash,
                    &apply_result,
                    split_state_roots,
                )?)
            } else {
                None
            };
            Ok(ApplyChunkResult::SameHeight(SameHeightResult {
                gas_limit,
                shard_uid: shard_info.shard_uid,
                apply_result,
                apply_split_result_or_state_changes,
            }))
        }
        Err(err) => Err(err),
    }
}

/// Returns the apply chunk job when applying an old chunk and applying transactions.
fn apply_old_chunk(
    parent_span: &tracing::Span,
    block_context: BlockContext,
    prev_chunk_extra: &ChunkExtra,
    shard_info: ShardInfo,
    state_patch: SandboxStatePatch,
    runtime: Arc<dyn RuntimeAdapter>,
    epoch_manager: Arc<dyn EpochManagerAdapter>,
    split_state_roots: Option<HashMap<ShardUId, CryptoHash>>,
) -> Result<ApplyChunkResult, Error> {
    let shard_id = shard_info.shard_uid.shard_id();
    let _span = tracing::debug_span!(
            target: "chain",
            parent: parent_span,
            "existing_chunk",
            shard_id)
    .entered();

    let storage_config = RuntimeStorageConfig {
        state_root: *prev_chunk_extra.state_root(),
        use_flat_storage: true,
        source: crate::types::StorageDataSource::Db,
        state_patch,
        record_storage: false,
    };
    match runtime.apply_transactions(
        shard_id,
        storage_config,
        block_context.height,
        block_context.block_timestamp,
        &block_context.prev_block_hash,
        &block_context.block_hash,
        &[],
        &[],
        prev_chunk_extra.validator_proposals(),
        block_context.gas_price,
        prev_chunk_extra.gas_limit(),
        &block_context.challenges_result,
        block_context.random_seed,
        false,
        false,
    ) {
        Ok(apply_result) => {
            let apply_split_result_or_state_changes = if shard_info.will_shard_layout_change {
                Some(apply_split_state_changes(
                    epoch_manager.as_ref(),
                    runtime.as_ref(),
                    &block_context.block_hash,
                    &block_context.prev_block_hash,
                    &apply_result,
                    split_state_roots,
                )?)
            } else {
                None
            };
            Ok(ApplyChunkResult::DifferentHeight(DifferentHeightResult {
                shard_uid: shard_info.shard_uid,
                apply_result,
                apply_split_result_or_state_changes,
            }))
        }
        Err(err) => Err(err),
    }
}

/// Returns the apply chunk job when just splitting state but not applying transactions.
fn apply_split_state_chunk(
    parent_span: &tracing::Span,
    block_context: BlockContext,
    shard_uid: ShardUId,
    runtime: Arc<dyn RuntimeAdapter>,
    epoch_manager: Arc<dyn EpochManagerAdapter>,
    split_state_roots: HashMap<ShardUId, CryptoHash>,
    state_changes: StateChangesForSplitStates,
) -> Result<ApplyChunkResult, Error> {
    let shard_id = shard_uid.shard_id();
    let _span = tracing::debug_span!(
        target: "chain",
        parent: parent_span,
        "split_state",
        shard_id,
        ?shard_uid)
    .entered();
    let next_epoch_shard_layout = epoch_manager.get_shard_layout(&block_context.next_epoch_id)?;
    let results = runtime.apply_update_to_split_states(
        &block_context.block_hash,
        split_state_roots,
        &next_epoch_shard_layout,
        state_changes,
    )?;
    Ok(ApplyChunkResult::SplitState(SplitStateResult { shard_uid, results }))
}

/// Process ApplyTransactionResult to apply changes to split states
/// When shards will change next epoch,
///    if `split_state_roots` is not None, that means states for the split shards are ready
///    this function updates these states and return apply results for these states
///    otherwise, this function returns state changes needed to be applied to split
///    states. These state changes will be stored in the database by `process_split_state`
fn apply_split_state_changes(
    epoch_manager: &dyn EpochManagerAdapter,
    runtime_adapter: &dyn RuntimeAdapter,
    block_hash: &CryptoHash,
    prev_block_hash: &CryptoHash,
    apply_result: &ApplyTransactionResult,
    split_state_roots: Option<HashMap<ShardUId, StateRoot>>,
) -> Result<ApplySplitStateResultOrStateChanges, Error> {
    let state_changes = StateChangesForSplitStates::from_raw_state_changes(
        apply_result.trie_changes.state_changes(),
        apply_result.processed_delayed_receipts.clone(),
    );
    let next_epoch_shard_layout = {
        let next_epoch_id = epoch_manager.get_next_epoch_id_from_prev_block(prev_block_hash)?;
        epoch_manager.get_shard_layout(&next_epoch_id)?
    };
    // split states are ready, apply update to them now
    if let Some(state_roots) = split_state_roots {
        let split_state_results = runtime_adapter.apply_update_to_split_states(
            block_hash,
            state_roots,
            &next_epoch_shard_layout,
            state_changes,
        )?;
        Ok(ApplySplitStateResultOrStateChanges::ApplySplitStateResults(split_state_results))
    } else {
        // split states are not ready yet, store state changes in consolidated_state_changes
        Ok(ApplySplitStateResultOrStateChanges::StateChangesForSplitStates(state_changes))
    }
}
