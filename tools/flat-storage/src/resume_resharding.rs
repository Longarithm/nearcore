use crate::commands::ResumeReshardingCmd;
use near_chain::ChainStore;
use near_chain::flat_storage_init::init_flat_storage;
use near_chain::resharding::flat_storage_resharder::FlatStorageResharder;
use near_chain::resharding::trie_state_resharder::TrieStateResharder;
use near_chain::types::{RuntimeAdapter, Tip};
use near_chain_configs::ReshardingHandle;
use near_epoch_manager::EpochManager;
use near_store::flat::{FlatStorageReshardingStatus, FlatStorageStatus, ParentSplitParameters};
use near_store::{ShardUId, StoreOpener};
use nearcore::{NearConfig, NightshadeRuntime, NightshadeRuntimeExt};
use std::path::PathBuf;

pub(crate) fn resume_resharding(
    cmd: &ResumeReshardingCmd,
    home_dir: &PathBuf,
    config: &NearConfig,
    opener: StoreOpener,
) -> anyhow::Result<()> {
    let node_storage = opener.open_in_mode(near_store::Mode::ReadWriteExisting).unwrap();
    let epoch_manager = EpochManager::new_arc_handle(
        node_storage.get_hot_store(),
        &config.genesis.config,
        Some(home_dir),
    );
    let runtime_adapter = NightshadeRuntime::from_config(
        home_dir,
        node_storage.get_hot_store(),
        &config,
        epoch_manager.clone(),
    )?;
    let chain_store = ChainStore::new(
        node_storage.get_hot_store(),
        false,
        config.genesis.config.transaction_validity_period,
    );
    let shard_uid = ShardUId::new(3, cmd.shard_id); // version is fixed at 3 in resharding V3
    let flat_storage_status =
        runtime_adapter.get_flat_storage_manager().get_flat_storage_status(shard_uid);
    let FlatStorageStatus::Resharding(FlatStorageReshardingStatus::SplittingParent(
        ParentSplitParameters { flat_head, .. },
    )) = flat_storage_status
    else {
        return Err(anyhow::anyhow!("Flat storage is not in resharding state"));
    };
    let block_header = chain_store.get_block_header(&flat_head.hash)?;
    let tip = Tip {
        height: flat_head.height,
        last_block_hash: flat_head.hash,
        prev_block_hash: flat_head.prev_hash,
        epoch_id: *block_header.epoch_id(),
        next_epoch_id: *block_header.next_epoch_id(),
    };
    init_flat_storage(&tip, epoch_manager.as_ref(), runtime_adapter.as_ref())?;
    let flat_storage_resharder = FlatStorageResharder::new(
        epoch_manager,
        runtime_adapter.clone(),
        ReshardingHandle::new(),
        config.client_config.resharding_config.clone(),
    );

    flat_storage_resharder.resume(shard_uid)?;

    tracing::info!(target: "resharding", "FlatStorageResharder completed");

    let trie_state_resharder = TrieStateResharder::new(
        runtime_adapter,
        ReshardingHandle::new(),
        config.client_config.resharding_config.clone(),
    );
    trie_state_resharder.resume(shard_uid)?;

    tracing::info!(target: "resharding", "TrieStateResharder completed");

    Ok(())
}
