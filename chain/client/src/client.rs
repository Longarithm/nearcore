//! Client is responsible for tracking the chain, blocks, chunks, and producing
//! them when needed.
//! This client works completely synchronously and must be operated by some async actor outside.

use crate::chunk_distribution_network::{ChunkDistributionClient, ChunkDistributionNetwork};
use crate::chunk_inclusion_tracker::ChunkInclusionTracker;
use crate::chunk_producer::ChunkProducer;
use crate::client_actor::ClientSenderForClient;
use crate::debug::BlockProductionTracker;
use crate::metrics;
use crate::stateless_validation::chunk_endorsement::ChunkEndorsementTracker;
use crate::stateless_validation::chunk_validator::ChunkValidator;
use crate::stateless_validation::partial_witness::partial_witness_actor::PartialWitnessSenderForClient;
use crate::sync::block::BlockSync;
use crate::sync::epoch::EpochSync;
use crate::sync::handler::SyncHandler;
use crate::sync::header::HeaderSync;
use crate::sync::state::chain_requests::ChainSenderForStateSync;
use crate::sync::state::{StateSync, StateSyncResult};
use itertools::Itertools;
use near_async::futures::{AsyncComputationSpawner, FutureSpawner};
use near_async::messaging::IntoSender;
use near_async::messaging::{CanSend, Sender};
use near_async::time::{Clock, Duration, Instant};
use near_chain::chain::{
    ApplyChunksDoneMessage, BlockCatchUpRequest, BlockMissingChunks, BlocksCatchUpState,
    VerifyBlockHashAndSignatureResult,
};
use near_chain::orphan::OrphanMissingChunks;
use near_chain::resharding::types::ReshardingSender;
use near_chain::state_snapshot_actor::SnapshotCallbacks;
use near_chain::test_utils::format_hash;
use near_chain::types::{ChainConfig, LatestKnown, RuntimeAdapter};
use near_chain::{
    BlockProcessingArtifact, BlockStatus, Chain, ChainGenesis, ChainStoreAccess, Doomslug,
    DoomslugThresholdMode, Provenance,
};
use near_chain_configs::{ClientConfig, MutableValidatorSigner, UpdatableClientConfig};
use near_chunks::adapter::ShardsManagerRequestFromClient;
use near_chunks::logic::{decode_encoded_chunk, persist_chunk};
use near_client_primitives::types::{Error, StateSyncStatus, SyncStatus};
use near_epoch_manager::EpochManagerAdapter;
use near_epoch_manager::shard_assignment::{account_id_to_shard_id, shard_id_to_uid};
use near_epoch_manager::shard_tracker::ShardTracker;
use near_network::client::ProcessTxResponse;
use near_network::types::{AccountKeys, ChainInfo, PeerManagerMessageRequest, SetChainInfo};
use near_network::types::{
    HighestHeightPeerInfo, NetworkRequests, PeerManagerAdapter, ReasonForBan,
};
use near_pool::InsertTransactionResult;
use near_primitives::block::{Approval, ApprovalInner, ApprovalMessage, Block, BlockHeader, Tip};
use near_primitives::block_header::ApprovalType;
use near_primitives::challenge::{Challenge, ChallengeBody};
use near_primitives::epoch_info::RngSeed;
use near_primitives::errors::EpochError;
use near_primitives::hash::CryptoHash;
use near_primitives::merkle::{MerklePath, PartialMerkleTree};
use near_primitives::network::PeerId;
use near_primitives::optimistic_block::OptimisticBlock;
use near_primitives::receipt::Receipt;
use near_primitives::sharding::{
    EncodedShardChunk, PartialEncodedChunk, ShardChunk, ShardChunkHeader, StateSyncInfo,
    StateSyncInfoV1,
};
use near_primitives::stateless_validation::ChunkProductionKey;
use near_primitives::transaction::SignedTransaction;
use near_primitives::types::{AccountId, ApprovalStake, BlockHeight, EpochId, NumBlocks, ShardId};
use near_primitives::unwrap_or_return;
use near_primitives::upgrade_schedule::ProtocolUpgradeVotingSchedule;
use near_primitives::utils::MaybeValidated;
use near_primitives::validator_signer::ValidatorSigner;
use near_primitives::version::PROTOCOL_VERSION;
use near_primitives::views::{CatchupStatusView, DroppedReason};
use std::cmp::max;
use std::collections::{HashMap, HashSet};
use std::num::NonZeroUsize;
use std::sync::Arc;
use tracing::{debug, debug_span, error, info, trace, warn};

#[cfg(feature = "test_features")]
use crate::chunk_producer::AdvProduceChunksMode;

const NUM_REBROADCAST_BLOCKS: usize = 30;

/// Drop blocks whose height are beyond head + horizon if it is not in the current epoch.
const BLOCK_HORIZON: u64 = 500;

/// number of blocks at the epoch start for which we will log more detailed info
pub const EPOCH_START_INFO_BLOCKS: u64 = 500;

/// Defines whether in case of adversarial block production invalid blocks can
/// be produced.
#[cfg(feature = "test_features")]
#[derive(PartialEq, Eq)]
pub enum AdvProduceBlocksMode {
    All,
    OnlyValid,
}

/// The state associated with downloading state for a shard this node will track in the
/// future but does not currently.
pub struct CatchupState {
    /// Manages downloading the state.
    pub state_sync: StateSync,
    /// Keeps track of state downloads, and gets passed to `state_sync`.
    pub sync_status: StateSyncStatus,
    /// Manages going back to apply chunks after state has been downloaded.
    pub catchup: BlocksCatchUpState,
}

pub struct Client {
    /// Adversarial controls - should be enabled only to test disruptive
    /// behavior on chain.
    #[cfg(feature = "test_features")]
    pub adv_produce_blocks: Option<AdvProduceBlocksMode>,

    /// Fast Forward accrued delta height used to calculate fast forwarded timestamps for each block.
    #[cfg(feature = "sandbox")]
    pub(crate) accrued_fastforward_delta: near_primitives::types::BlockHeightDelta,

    pub clock: Clock,
    pub config: ClientConfig,
    pub chain: Chain,
    pub doomslug: Doomslug,
    pub epoch_manager: Arc<dyn EpochManagerAdapter>,
    pub shard_tracker: ShardTracker,
    pub runtime_adapter: Arc<dyn RuntimeAdapter>,
    pub shards_manager_adapter: Sender<ShardsManagerRequestFromClient>,
    /// Network adapter.
    pub network_adapter: PeerManagerAdapter,
    /// Signer for block producer (if present). This field is mutable and optional. Use with caution!
    /// Lock the value of mutable validator signer for the duration of a request to ensure consistency.
    /// Please note that the locked value should not be stored anywhere or passed through the thread boundary.
    pub validator_signer: MutableValidatorSigner,
    /// Approvals for which we do not have the block yet
    pub pending_approvals:
        lru::LruCache<ApprovalInner, HashMap<AccountId, (Approval, ApprovalType)>>,
    /// Handles syncing chain to the actual state of the network.
    pub sync_handler: SyncHandler,
    /// A mapping from a block for which a state sync is underway for the next epoch, and the object
    /// storing the current status of the state sync and blocks catch up
    pub catchup_state_syncs: HashMap<CryptoHash, CatchupState>,
    /// Spawns async tasks for catchup state sync.
    state_sync_future_spawner: Arc<dyn FutureSpawner>,
    /// Sender for catchup state sync requests.
    chain_sender_for_state_sync: ChainSenderForStateSync,
    // Sender to be able to send a message to myself.
    pub myself_sender: ClientSenderForClient,
    /// List of currently accumulated challenges.
    pub challenges: HashMap<CryptoHash, Challenge>,
    /// Blocks that have been re-broadcast recently. They should not be broadcast again.
    rebroadcasted_blocks: lru::LruCache<CryptoHash, ()>,
    /// Last time the head was updated, or our head was rebroadcasted. Used to re-broadcast the head
    /// again to prevent network from stalling if a large percentage of the network missed a block
    last_time_head_progress_made: Instant,
    /// Block production timing information. Used only for debug purposes.
    /// Stores approval information and production time of the block
    pub block_production_info: BlockProductionTracker,
    /// Cached precomputed set of TIER1 accounts.
    /// See send_network_chain_info().
    tier1_accounts_cache: Option<(EpochId, Arc<AccountKeys>)>,
    /// Resharding sender.
    pub resharding_sender: ReshardingSender,
    /// Helper module for handling chunk production.
    pub chunk_producer: ChunkProducer,
    /// Helper module for stateless validation functionality like chunk witness production, validation
    /// chunk endorsements tracking etc.
    pub chunk_validator: ChunkValidator,
    /// Tracks current chunks that are ready to be included in block
    /// Also tracks banned chunk producers and filters out chunks produced by them
    pub chunk_inclusion_tracker: ChunkInclusionTracker,
    /// Tracks chunk endorsements received from chunk validators. Used to filter out chunks ready for inclusion
    pub chunk_endorsement_tracker: ChunkEndorsementTracker,
    /// Adapter to send request to partial_witness_actor to distribute state witness.
    pub partial_witness_adapter: PartialWitnessSenderForClient,
    // Optional value used for the Chunk Distribution Network Feature.
    chunk_distribution_network: Option<ChunkDistributionNetwork>,
    /// Upgrade schedule which determines when the client starts voting for new protocol versions.
    upgrade_schedule: ProtocolUpgradeVotingSchedule,
    /// Produced optimistic block.
    last_optimistic_block_produced: Option<OptimisticBlock>,
}

impl AsRef<Client> for Client {
    fn as_ref(&self) -> &Client {
        self
    }
}

impl Client {
    pub(crate) fn update_client_config(&self, update_client_config: UpdatableClientConfig) -> bool {
        let mut is_updated = false;
        is_updated |= self.config.expected_shutdown.update(update_client_config.expected_shutdown);
        is_updated |= self.config.resharding_config.update(update_client_config.resharding_config);
        is_updated |= self
            .config
            .produce_chunk_add_transactions_time_limit
            .update(update_client_config.produce_chunk_add_transactions_time_limit);
        is_updated
    }

    /// Updates client's mutable validator signer.
    /// It will update all validator signers that synchronize with it.
    pub(crate) fn update_validator_signer(&self, signer: Option<Arc<ValidatorSigner>>) -> bool {
        self.validator_signer.update(signer)
    }
}

impl Client {
    pub fn new(
        clock: Clock,
        config: ClientConfig,
        chain_genesis: ChainGenesis,
        epoch_manager: Arc<dyn EpochManagerAdapter>,
        shard_tracker: ShardTracker,
        runtime_adapter: Arc<dyn RuntimeAdapter>,
        network_adapter: PeerManagerAdapter,
        shards_manager_sender: Sender<ShardsManagerRequestFromClient>,
        validator_signer: MutableValidatorSigner,
        enable_doomslug: bool,
        rng_seed: RngSeed,
        snapshot_callbacks: Option<SnapshotCallbacks>,
        async_computation_spawner: Arc<dyn AsyncComputationSpawner>,
        partial_witness_adapter: PartialWitnessSenderForClient,
        resharding_sender: ReshardingSender,
        state_sync_future_spawner: Arc<dyn FutureSpawner>,
        chain_sender_for_state_sync: ChainSenderForStateSync,
        myself_sender: ClientSenderForClient,
        upgrade_schedule: ProtocolUpgradeVotingSchedule,
    ) -> Result<Self, Error> {
        let doomslug_threshold_mode = if enable_doomslug {
            DoomslugThresholdMode::TwoThirds
        } else {
            DoomslugThresholdMode::NoApprovals
        };
        let chain_config = ChainConfig {
            save_trie_changes: config.save_trie_changes,
            background_migration_threads: config.client_background_migration_threads,
            resharding_config: config.resharding_config.clone(),
        };
        let chain = Chain::new(
            clock.clone(),
            epoch_manager.clone(),
            shard_tracker.clone(),
            runtime_adapter.clone(),
            &chain_genesis,
            doomslug_threshold_mode,
            chain_config,
            snapshot_callbacks,
            async_computation_spawner.clone(),
            validator_signer.clone(),
            resharding_sender.clone(),
        )?;
        chain.init_flat_storage()?;
        let epoch_sync = EpochSync::new(
            clock.clone(),
            network_adapter.clone(),
            chain.genesis().clone(),
            async_computation_spawner.clone(),
            config.epoch_sync.clone(),
            &chain.chain_store.store(),
        );
        let header_sync = HeaderSync::new(
            clock.clone(),
            network_adapter.clone(),
            config.header_sync_initial_timeout,
            config.header_sync_progress_timeout,
            config.header_sync_stall_ban_timeout,
            config.header_sync_expected_height_per_second,
            config.expected_shutdown.clone(),
        );
        let block_sync = BlockSync::new(
            clock.clone(),
            network_adapter.clone(),
            config.block_fetch_horizon,
            config.archive,
            config.state_sync_enabled,
        );

        let state_sync = StateSync::new(
            clock.clone(),
            runtime_adapter.store().clone(),
            epoch_manager.clone(),
            runtime_adapter.clone(),
            network_adapter.clone().into_sender(),
            config.state_sync_external_timeout,
            config.state_sync_p2p_timeout,
            config.state_sync_retry_backoff,
            config.state_sync_external_backoff,
            &config.chain_id,
            &config.state_sync.sync,
            chain_sender_for_state_sync.clone(),
            state_sync_future_spawner.clone(),
            false,
        );
        let num_block_producer_seats = config.num_block_producer_seats as usize;

        let doomslug = Doomslug::new(
            clock.clone(),
            chain.chain_store().largest_target_height()?,
            config.min_block_production_delay,
            config.max_block_production_delay,
            config.max_block_production_delay / 10,
            config.max_block_wait_delay,
            config.chunk_wait_mult,
            doomslug_threshold_mode,
        );
        let chunk_endorsement_tracker =
            ChunkEndorsementTracker::new(epoch_manager.clone(), chain.chain_store().store());
        let chunk_producer = ChunkProducer::new(
            clock.clone(),
            config.produce_chunk_add_transactions_time_limit.clone(),
            &chain.chain_store(),
            epoch_manager.clone(),
            runtime_adapter.clone(),
            rng_seed,
            config.transaction_pool_size_limit,
        );
        let chunk_validator = ChunkValidator::new(
            epoch_manager.clone(),
            network_adapter.clone().into_sender(),
            runtime_adapter.clone(),
            config.orphan_state_witness_pool_size,
            async_computation_spawner,
        );
        let chunk_distribution_network = ChunkDistributionNetwork::from_config(&config);
        Ok(Self {
            #[cfg(feature = "test_features")]
            adv_produce_blocks: None,
            #[cfg(feature = "sandbox")]
            accrued_fastforward_delta: 0,
            clock: clock.clone(),
            config: config.clone(),
            chain,
            doomslug,
            epoch_manager,
            shard_tracker,
            runtime_adapter,
            shards_manager_adapter: shards_manager_sender,
            network_adapter,
            validator_signer,
            pending_approvals: lru::LruCache::new(
                NonZeroUsize::new(num_block_producer_seats).unwrap(),
            ),
            sync_handler: SyncHandler::new(
                clock.clone(),
                config,
                epoch_sync,
                header_sync,
                state_sync,
                block_sync,
            ),
            catchup_state_syncs: HashMap::new(),
            state_sync_future_spawner,
            chain_sender_for_state_sync,
            myself_sender,
            challenges: Default::default(),
            rebroadcasted_blocks: lru::LruCache::new(
                NonZeroUsize::new(NUM_REBROADCAST_BLOCKS).unwrap(),
            ),
            last_time_head_progress_made: clock.now(),
            block_production_info: BlockProductionTracker::new(),
            tier1_accounts_cache: None,
            resharding_sender,
            chunk_producer,
            chunk_validator,
            chunk_inclusion_tracker: ChunkInclusionTracker::new(),
            chunk_endorsement_tracker,
            partial_witness_adapter,
            chunk_distribution_network,
            upgrade_schedule,
            last_optimistic_block_produced: None,
        })
    }

    // Checks if it's been at least `stall_timeout` since the last time the head was updated, or
    // this method was called. If yes, rebroadcasts the current head.
    pub fn check_head_progress_stalled(&mut self, stall_timeout: Duration) -> Result<(), Error> {
        if self.clock.now() > self.last_time_head_progress_made + stall_timeout
            && !self.sync_handler.sync_status.is_syncing()
        {
            let block = self.chain.get_block(&self.chain.head()?.last_block_hash)?;
            self.network_adapter.send(PeerManagerMessageRequest::NetworkRequests(
                NetworkRequests::Block { block: block },
            ));
            self.last_time_head_progress_made = self.clock.now();
        }
        Ok(())
    }

    pub fn remove_transactions_for_block(
        &mut self,
        me: AccountId,
        block: &Block,
    ) -> Result<(), Error> {
        let epoch_id = self.epoch_manager.get_epoch_id(block.hash())?;
        let shard_layout = self.epoch_manager.get_shard_layout(&epoch_id)?;
        for (shard_index, chunk_header) in block.chunks().iter_deprecated().enumerate() {
            let shard_id = shard_layout.get_shard_id(shard_index);
            let shard_id = shard_id.map_err(Into::<EpochError>::into)?;
            let shard_uid = shard_id_to_uid(self.epoch_manager.as_ref(), shard_id, &epoch_id)?;
            if block.header().height() == chunk_header.height_included() {
                if self.shard_tracker.cares_about_shard_this_or_next_epoch(
                    Some(&me),
                    block.header().prev_hash(),
                    shard_id,
                    true,
                ) {
                    // By now the chunk must be in store, otherwise the block would have been orphaned
                    let chunk = self.chain.get_chunk(&chunk_header.chunk_hash()).unwrap();
                    let transactions = chunk.transactions();
                    self.chunk_producer
                        .sharded_tx_pool
                        .remove_transactions(shard_uid, transactions);
                }
            }
        }
        for challenge in block.challenges().iter() {
            self.challenges.remove(&challenge.hash);
        }
        Ok(())
    }

    pub fn reintroduce_transactions_for_block(
        &mut self,
        me: AccountId,
        block: &Block,
    ) -> Result<(), Error> {
        let epoch_id = self.epoch_manager.get_epoch_id(block.hash())?;
        let shard_layout = self.epoch_manager.get_shard_layout(&epoch_id)?;

        for (shard_index, chunk_header) in block.chunks().iter_deprecated().enumerate() {
            let shard_id = shard_layout.get_shard_id(shard_index);
            let shard_id = shard_id.map_err(Into::<EpochError>::into)?;
            let shard_uid = shard_id_to_uid(self.epoch_manager.as_ref(), shard_id, &epoch_id)?;

            if block.header().height() == chunk_header.height_included() {
                if self.shard_tracker.cares_about_shard_this_or_next_epoch(
                    Some(&me),
                    block.header().prev_hash(),
                    shard_id,
                    false,
                ) {
                    // By now the chunk must be in store, otherwise the block would have been orphaned
                    let chunk = self.chain.get_chunk(&chunk_header.chunk_hash()).unwrap();
                    let reintroduced_count = self
                        .chunk_producer
                        .sharded_tx_pool
                        .reintroduce_transactions(shard_uid, chunk.transactions().to_vec());
                    if reintroduced_count < chunk.transactions().len() {
                        debug!(target: "client",
                            reintroduced_count,
                            num_tx = chunk.transactions().len(),
                            "Reintroduced transactions");
                    }
                }
            }
        }
        for challenge in block.challenges().iter() {
            self.challenges.insert(challenge.hash, challenge.clone());
        }
        Ok(())
    }

    /// Checks couple conditions whether Client can produce new block on height
    /// `height` on top of block with `prev_header`.
    /// Needed to skip several checks in case of adversarial controls enabled.
    /// TODO: consider returning `Result<(), Error>` as `Ok(false)` looks like
    /// faulty logic.
    fn can_produce_block(
        &self,
        prev_header: &BlockHeader,
        height: BlockHeight,
        account_id: &AccountId,
        next_block_proposer: &AccountId,
    ) -> Result<bool, Error> {
        #[cfg(feature = "test_features")]
        {
            if self.adv_produce_blocks == Some(AdvProduceBlocksMode::All) {
                return Ok(true);
            }
        }

        // If we are not block proposer, skip block production.
        if account_id != next_block_proposer {
            info!(target: "client", height, "Skipping block production, not block producer for next block.");
            return Ok(false);
        }

        #[cfg(feature = "test_features")]
        {
            if self.adv_produce_blocks == Some(AdvProduceBlocksMode::OnlyValid) {
                return Ok(true);
            }
        }

        // If height is known already, don't produce new block for this height.
        let known_height = self.chain.chain_store().get_latest_known()?.height;
        if height <= known_height {
            return Ok(false);
        }

        // If we are to start new epoch with this block, check if the previous
        // block is caught up. If it is not the case, we wouldn't be able to
        // apply the following block, so we also skip block production.
        let prev_hash = prev_header.hash();
        if self.epoch_manager.is_next_block_epoch_start(prev_hash)? {
            let prev_prev_hash = prev_header.prev_hash();
            if !self.chain.prev_block_is_caught_up(prev_prev_hash, prev_hash)? {
                debug!(target: "client", height, "Skipping block production, prev block is not caught up");
                return Ok(false);
            }
        }

        Ok(true)
    }

    fn pre_block_production_check(
        &self,
        prev_header: &BlockHeader,
        height: BlockHeight,
        validator_signer: &Arc<ValidatorSigner>,
    ) -> Result<(), Error> {
        // Check that we are were called at the block that we are producer for.
        let epoch_id =
            self.epoch_manager.get_epoch_id_from_prev_block(&prev_header.hash()).unwrap();
        let next_block_proposer = self.epoch_manager.get_block_producer(&epoch_id, height)?;

        let protocol_version = self
            .epoch_manager
            .get_epoch_protocol_version(&epoch_id)
            .expect("Epoch info should be ready at this point");
        if protocol_version > PROTOCOL_VERSION {
            panic!(
                "The client protocol version is older than the protocol version of the network. Please update nearcore. Client protocol version:{}, network protocol version {}",
                PROTOCOL_VERSION, protocol_version
            );
        }

        if !self.can_produce_block(
            &prev_header,
            height,
            validator_signer.validator_id(),
            &next_block_proposer,
        )? {
            debug!(target: "client", me=?validator_signer.validator_id(), ?next_block_proposer, "Should reschedule block");
            return Err(Error::BlockProducer("Should reschedule".to_string()));
        }

        let validator_stake =
            self.epoch_manager.get_validator_by_account_id(&epoch_id, &next_block_proposer)?;

        let validator_pk = validator_stake.take_public_key();
        if validator_pk != validator_signer.public_key() {
            debug!(target: "client",
                local_validator_key = ?validator_signer.public_key(),
                ?validator_pk,
                "Local validator key does not match expected validator key, skipping optimistic block production");
            let err = Error::BlockProducer("Local validator key mismatch".to_string());
            #[cfg(not(feature = "test_features"))]
            return Err(err);
            #[cfg(feature = "test_features")]
            match self.adv_produce_blocks {
                None | Some(AdvProduceBlocksMode::OnlyValid) => return Err(err),
                Some(AdvProduceBlocksMode::All) => {}
            }
        }
        Ok(())
    }

    pub fn is_optimistic_block_done(&self, next_height: BlockHeight) -> bool {
        self.last_optimistic_block_produced
            .as_ref()
            .filter(|ob| ob.inner.block_height == next_height)
            .is_some()
    }

    pub fn save_optimistic_block(&mut self, optimistic_block: &OptimisticBlock) {
        if let Some(old_block) = self.last_optimistic_block_produced.as_ref() {
            if old_block.inner.block_height == optimistic_block.inner.block_height {
                warn!(target: "client",
                    height=old_block.inner.block_height,
                    old_previous_hash=?old_block.inner.prev_block_hash,
                    new_previous_hash=?optimistic_block.inner.prev_block_hash,
                    "Optimistic block already exists, replacing");
            }
        }
        self.last_optimistic_block_produced = Some(optimistic_block.clone());
    }

    /// Produce optimistic block for given `height` on top of chain head.
    /// Either returns optimistic block or error.
    pub fn produce_optimistic_block_on_head(
        &mut self,
        height: BlockHeight,
    ) -> Result<Option<OptimisticBlock>, Error> {
        let _span =
            tracing::debug_span!(target: "client", "produce_optimistic_block_on_head", height)
                .entered();

        let head = self.chain.head()?;
        assert_eq!(
            head.epoch_id,
            self.epoch_manager.get_epoch_id_from_prev_block(&head.prev_block_hash).unwrap()
        );

        let prev_hash = head.last_block_hash;
        let prev_header = self.chain.get_block_header(&prev_hash)?;

        let validator_signer: Arc<ValidatorSigner> =
            self.validator_signer.get().ok_or_else(|| {
                Error::BlockProducer("Called without block producer info.".to_string())
            })?;

        if let Err(err) = self.pre_block_production_check(&prev_header, height, &validator_signer) {
            debug!(target: "client", height, ?err, "Skipping optimistic block production.");
            return Ok(None);
        }

        debug!(
            target: "client",
            validator=?validator_signer.validator_id(),
            height=height,
            prev_height=prev_header.height(),
            prev_hash=format_hash(prev_hash),
            "Producing optimistic block",
        );

        #[cfg(feature = "sandbox")]
        let sandbox_delta_time = Some(self.sandbox_delta_time());
        #[cfg(not(feature = "sandbox"))]
        let sandbox_delta_time = None;

        // TODO(#10584): Add debug information about the block production in self.block_production_info

        let optimistic_block = OptimisticBlock::produce(
            &prev_header,
            height,
            &*validator_signer,
            self.clock.clone(),
            sandbox_delta_time,
        );

        metrics::OPTIMISTIC_BLOCK_PRODUCED_TOTAL.inc();

        Ok(Some(optimistic_block))
    }

    /// Produce block if we are block producer for given block `height`.
    /// Either returns produced block (not applied) or error.
    pub fn produce_block(&mut self, height: BlockHeight) -> Result<Option<Block>, Error> {
        self.produce_block_on_head(height, true)
    }

    /// Produce block for given `height` on top of chain head.
    /// Either returns produced block (not applied) or error.
    pub fn produce_block_on_head(
        &mut self,
        height: BlockHeight,
        prepare_chunk_headers: bool,
    ) -> Result<Option<Block>, Error> {
        let _span =
            tracing::debug_span!(target: "client", "produce_block_on_head", height).entered();

        let head = self.chain.head()?;
        assert_eq!(
            head.epoch_id,
            self.epoch_manager.get_epoch_id_from_prev_block(&head.prev_block_hash).unwrap()
        );

        if prepare_chunk_headers {
            self.chunk_inclusion_tracker.prepare_chunk_headers_ready_for_inclusion(
                &head.last_block_hash,
                &mut self.chunk_endorsement_tracker,
            )?;
        }

        self.produce_block_on(height, head.last_block_hash)
    }

    /// Produce block for given `height` on top of block `prev_hash`.
    /// Should be called either from `produce_block` or in tests.
    pub fn produce_block_on(
        &mut self,
        height: BlockHeight,
        prev_hash: CryptoHash,
    ) -> Result<Option<Block>, Error> {
        let validator_signer = self.validator_signer.get().ok_or_else(|| {
            Error::BlockProducer("Called without block producer info.".to_string())
        })?;
        let optimistic_block = self
            .last_optimistic_block_produced
            .as_ref()
            .filter(|ob| {
                // Make sure that the optimistic block is produced on the same previous block.
                if ob.inner.prev_block_hash == prev_hash {
                    return true;
                }
                warn!(target: "client",
                    height=height,
                    prev_hash=?prev_hash,
                    optimistic_block_prev_hash=?ob.inner.prev_block_hash,
                    "Optimistic block was constructed on different block, discarding it");
                false
            })
            .cloned();
        // Check that we are were called at the block that we are producer for.
        let epoch_id = self.epoch_manager.get_epoch_id_from_prev_block(&prev_hash).unwrap();

        let prev = self.chain.get_block_header(&prev_hash)?;
        let prev_height = prev.height();
        let prev_epoch_id = *prev.epoch_id();
        let prev_next_bp_hash = *prev.next_bp_hash();

        if let Err(err) = self.pre_block_production_check(&prev, height, &validator_signer) {
            debug!(target: "client", height, ?err, "Skipping block production");
            return Ok(None);
        }

        // Check and update the doomslug tip here. This guarantees that our endorsement will be in the
        // doomslug witness. Have to do it before checking the ability to produce a block.
        let _ = self.check_and_update_doomslug_tip()?;

        let new_chunks = self
            .chunk_inclusion_tracker
            .get_chunk_headers_ready_for_inclusion(&epoch_id, &prev_hash);
        debug!(
            target: "client",
            validator=?validator_signer.validator_id(),
            height=height,
            prev_height=prev.height(),
            prev_hash=format_hash(prev_hash),
            new_chunks_count=new_chunks.len(),
            new_chunks=?new_chunks.values().collect_vec(),
            "Producing block",
        );

        // If we are producing empty blocks and there are no transactions.
        if !self.config.produce_empty_blocks && new_chunks.is_empty() {
            debug!(target: "client", "Empty blocks, skipping block production");
            return Ok(None);
        }

        let mut approvals_map = self.doomslug.get_witness(&prev_hash, prev_height, height);

        // At this point, the previous epoch hash must be available
        let epoch_id = self
            .epoch_manager
            .get_epoch_id_from_prev_block(&prev_hash)
            .expect("Epoch hash should exist at this point");
        let protocol_version = self
            .epoch_manager
            .get_epoch_protocol_version(&epoch_id)
            .expect("Epoch info should be ready at this point");
        if protocol_version > PROTOCOL_VERSION {
            panic!(
                "The client protocol version is older than the protocol version of the network. Please update nearcore. Client protocol version:{}, network protocol version {}",
                PROTOCOL_VERSION, protocol_version
            );
        }

        let approvals = self
            .epoch_manager
            .get_epoch_block_approvers_ordered(&prev_hash)?
            .into_iter()
            .map(|ApprovalStake { account_id, .. }| {
                approvals_map.remove(&account_id).map(|x| x.0.signature.into())
            })
            .collect();

        debug_assert_eq!(approvals_map.len(), 0);

        let next_epoch_id = self
            .epoch_manager
            .get_next_epoch_id_from_prev_block(&prev_hash)
            .expect("Epoch hash should exist at this point");

        let protocol_version = self.epoch_manager.get_epoch_protocol_version(&epoch_id)?;
        let gas_price_adjustment_rate =
            self.chain.block_economics_config.gas_price_adjustment_rate(protocol_version);
        let min_gas_price = self.chain.block_economics_config.min_gas_price(protocol_version);
        let max_gas_price = self.chain.block_economics_config.max_gas_price(protocol_version);

        let next_bp_hash = if prev_epoch_id != epoch_id {
            Chain::compute_bp_hash(self.epoch_manager.as_ref(), next_epoch_id, epoch_id)?
        } else {
            prev_next_bp_hash
        };

        #[cfg(feature = "sandbox")]
        let sandbox_delta_time = Some(self.sandbox_delta_time());
        #[cfg(not(feature = "sandbox"))]
        let sandbox_delta_time = None;

        // Get block extra from previous block.
        let block_merkle_tree = self.chain.chain_store().get_block_merkle_tree(&prev_hash)?;
        let mut block_merkle_tree = PartialMerkleTree::clone(&block_merkle_tree);
        block_merkle_tree.insert(prev_hash);
        let block_merkle_root = block_merkle_tree.root();
        // The number of leaves in Block Merkle Tree is the amount of Blocks on the Canonical Chain by construction.
        // The ordinal of the next Block will be equal to this amount plus one.
        let block_ordinal: NumBlocks = block_merkle_tree.size() + 1;
        let prev_block_extra = self.chain.get_block_extra(&prev_hash)?;
        let prev_block = self.chain.get_block(&prev_hash)?;
        let mut chunk_headers =
            Chain::get_prev_chunk_headers(self.epoch_manager.as_ref(), &prev_block)?;
        let mut chunk_endorsements = vec![vec![]; chunk_headers.len()];

        // Add debug information about the block production (and info on when did the chunks arrive).
        self.block_production_info.record_block_production(
            height,
            BlockProductionTracker::construct_chunk_collection_info(
                height,
                &epoch_id,
                chunk_headers.len(),
                &new_chunks,
                self.epoch_manager.as_ref(),
                &self.chunk_inclusion_tracker,
            )?,
        );

        // Collect new chunk headers and endorsements.
        let shard_layout = self.epoch_manager.get_shard_layout(&epoch_id)?;
        for (shard_id, chunk_hash) in new_chunks {
            let shard_index =
                shard_layout.get_shard_index(shard_id).map_err(Into::<EpochError>::into)?;
            let (mut chunk_header, chunk_endorsement) =
                self.chunk_inclusion_tracker.get_chunk_header_and_endorsements(&chunk_hash)?;
            *chunk_header.height_included_mut() = height;
            *chunk_headers
                .get_mut(shard_index)
                .ok_or(near_chain_primitives::Error::InvalidShardId(shard_id))? = chunk_header;
            *chunk_endorsements
                .get_mut(shard_index)
                .ok_or(near_chain_primitives::Error::InvalidShardId(shard_id))? = chunk_endorsement;
        }

        let prev_header = &prev_block.header();

        let next_epoch_id = self.epoch_manager.get_next_epoch_id_from_prev_block(&prev_hash)?;

        let minted_amount = if self.epoch_manager.is_next_block_epoch_start(&prev_hash)? {
            Some(self.epoch_manager.get_epoch_info(&next_epoch_id)?.minted_amount())
        } else {
            None
        };

        let epoch_sync_data_hash = if self.epoch_manager.is_next_block_epoch_start(&prev_hash)? {
            let last_block_info = self.epoch_manager.get_block_info(prev_block.hash())?;
            let prev_epoch_id = *last_block_info.epoch_id();
            let prev_epoch_first_block_info =
                self.epoch_manager.get_block_info(last_block_info.epoch_first_block())?;
            let prev_epoch_prev_last_block_info =
                self.epoch_manager.get_block_info(last_block_info.prev_hash())?;
            let prev_epoch_info = self.epoch_manager.get_epoch_info(&prev_epoch_id)?;
            let cur_epoch_info = self.epoch_manager.get_epoch_info(&epoch_id)?;
            let next_epoch_info = self.epoch_manager.get_epoch_info(&next_epoch_id)?;
            Some(CryptoHash::hash_borsh(&(
                prev_epoch_first_block_info,
                prev_epoch_prev_last_block_info,
                last_block_info,
                prev_epoch_info,
                cur_epoch_info,
                next_epoch_info,
            )))
        } else {
            None
        };

        // Get all the current challenges.
        // TODO(2445): Enable challenges when they are working correctly.
        // let challenges = self.challenges.drain().map(|(_, challenge)| challenge).collect();
        let this_epoch_protocol_version =
            self.epoch_manager.get_epoch_protocol_version(&epoch_id)?;
        let next_epoch_protocol_version =
            self.epoch_manager.get_epoch_protocol_version(&next_epoch_id)?;

        let block = Block::produce(
            this_epoch_protocol_version,
            next_epoch_protocol_version,
            self.upgrade_schedule
                .protocol_version_to_vote_for(self.clock.now_utc(), next_epoch_protocol_version),
            prev_header,
            height,
            block_ordinal,
            chunk_headers,
            chunk_endorsements,
            epoch_id,
            next_epoch_id,
            epoch_sync_data_hash,
            approvals,
            gas_price_adjustment_rate,
            min_gas_price,
            max_gas_price,
            minted_amount,
            prev_block_extra.challenges_result.clone(),
            vec![],
            &*validator_signer,
            next_bp_hash,
            block_merkle_root,
            self.clock.clone(),
            sandbox_delta_time,
            optimistic_block,
        );

        // Update latest known even before returning block out, to prevent race conditions.
        self.chain
            .mut_chain_store()
            .save_latest_known(LatestKnown { height, seen: block.header().raw_timestamp() })?;

        metrics::BLOCK_PRODUCED_TOTAL.inc();

        Ok(Some(block))
    }

    fn send_challenges(
        &mut self,
        challenges: Vec<ChallengeBody>,
        signer: &Option<Arc<ValidatorSigner>>,
    ) {
        if let Some(validator_signer) = &signer {
            for body in challenges {
                let challenge = Challenge::produce(body, &**validator_signer);
                self.challenges.insert(challenge.hash, challenge.clone());
                self.network_adapter.send(PeerManagerMessageRequest::NetworkRequests(
                    NetworkRequests::Challenge(challenge),
                ));
            }
        }
    }

    /// Processes received block. Ban peer if the block header is invalid or the block is ill-formed.
    // This function is just a wrapper for process_block_impl that makes error propagation easier.
    pub fn receive_block(
        &mut self,
        block: Block,
        peer_id: PeerId,
        was_requested: bool,
        apply_chunks_done_sender: Option<Sender<ApplyChunksDoneMessage>>,
        signer: &Option<Arc<ValidatorSigner>>,
    ) {
        let hash = *block.hash();
        let prev_hash = *block.header().prev_hash();
        let _span = tracing::debug_span!(
            target: "client",
            "receive_block",
            me = ?signer.as_ref().map(|vs| vs.validator_id()),
            %prev_hash,
            %hash,
            height = block.header().height(),
            %peer_id,
            was_requested)
        .entered();

        let res = self.receive_block_impl(
            block,
            peer_id,
            was_requested,
            apply_chunks_done_sender,
            signer,
        );
        // Log the errors here. Note that the real error handling logic is already
        // done within process_block_impl, this is just for logging.
        if let Err(err) = res {
            if err.is_bad_data() {
                warn!(target: "client", ?err, "Receive bad block");
            } else if err.is_error() {
                if let near_chain::Error::DBNotFoundErr(msg) = &err {
                    debug_assert!(!msg.starts_with("BLOCK HEIGHT"), "{:?}", err);
                }
                if self.sync_handler.sync_status.is_syncing() {
                    // While syncing, we may receive blocks that are older or from next epochs.
                    // This leads to Old Block or EpochOutOfBounds errors.
                    debug!(target: "client", ?err, sync_status = ?self.sync_handler.sync_status, "Error receiving a block. is syncing");
                } else {
                    error!(target: "client", ?err, "Error on receiving a block. Not syncing");
                }
            } else {
                debug!(target: "client", ?err, "Process block: refused by chain");
            }
            self.chain.blocks_delay_tracker.mark_block_errored(&hash, err.to_string());
        }
    }

    /// Processes received block.
    /// This function first does some pre-check based on block height to avoid processing
    /// blocks multiple times.
    /// Then it process the block header. If the header if valid, broadcast the block to its peers
    /// Then it starts the block processing process to process the full block.
    pub fn receive_block_impl(
        &mut self,
        block: Block,
        peer_id: PeerId,
        was_requested: bool,
        apply_chunks_done_sender: Option<Sender<ApplyChunksDoneMessage>>,
        signer: &Option<Arc<ValidatorSigner>>,
    ) -> Result<(), near_chain::Error> {
        let _span =
            debug_span!(target: "chain", "receive_block_impl", was_requested, ?peer_id).entered();
        self.chain.blocks_delay_tracker.mark_block_received(&block);
        // To protect ourselves from spamming, we do some pre-check on block height before we do any
        // real processing.
        if !self.check_block_height(&block, was_requested)? {
            self.chain
                .blocks_delay_tracker
                .mark_block_dropped(block.hash(), DroppedReason::HeightProcessed);
            return Ok(());
        }

        // Before we proceed with any further processing, we first check that the block
        // hash and signature matches to make sure the block is indeed produced by the assigned
        // block producer. If not, we drop the block immediately and ban the peer
        if self.chain.verify_block_hash_and_signature(&block)?
            == VerifyBlockHashAndSignatureResult::Incorrect
        {
            self.ban_peer(peer_id, ReasonForBan::BadBlockHeader);
            return Err(near_chain::Error::InvalidSignature);
        }

        let prev_hash = *block.header().prev_hash();
        let block = block.into();
        self.verify_and_rebroadcast_block(&block, was_requested, &peer_id)?;
        let provenance =
            if was_requested { near_chain::Provenance::SYNC } else { near_chain::Provenance::NONE };
        let res = self.start_process_block(block, provenance, apply_chunks_done_sender, signer);
        match &res {
            Err(near_chain::Error::Orphan) => {
                debug!(target: "chain", ?prev_hash, "Orphan error");
                if !self.chain.is_orphan(&prev_hash) {
                    debug!(target: "chain", "not orphan");
                    self.request_block(prev_hash, peer_id)
                }
            }
            err => {
                debug!(target: "chain", ?err, "some other error");
            }
        }
        res
    }

    /// Check optimistic block and start processing if is valid.
    pub fn receive_optimistic_block(&mut self, block: OptimisticBlock, peer_id: &PeerId) {
        let _span = debug_span!(target: "client", "receive_optimistic_block").entered();
        debug!(target: "client", ?block, ?peer_id, "Received optimistic block");

        // Pre-validate the optimistic block.
        if let Err(e) = self.chain.pre_check_optimistic_block(&block) {
            near_chain::metrics::NUM_INVALID_OPTIMISTIC_BLOCKS.inc();
            debug!(target: "client", ?e, "Optimistic block is invalid");
            return;
        }

        if let Err(_) = self.chain.get_block_header(&block.prev_block_hash()) {
            // If the previous block is not found, add the block to the orphan
            // pool for now.
            self.chain.save_optimistic_orphan(block);
            return;
        }

        let signer = self.validator_signer.get();
        let me = signer.as_ref().map(|signer| signer.validator_id().clone());
        self.chain.preprocess_optimistic_block(
            block,
            &me,
            Some(self.myself_sender.apply_chunks_done.clone()),
        );
    }

    /// To protect ourselves from spamming, we do some pre-check on block height before we do any
    /// processing. This function returns true if the block height is valid.
    fn check_block_height(
        &self,
        block: &Block,
        was_requested: bool,
    ) -> Result<bool, near_chain::Error> {
        let head = self.chain.head()?;
        let is_syncing = self.sync_handler.sync_status.is_syncing();
        if block.header().height() >= head.height + BLOCK_HORIZON && is_syncing && !was_requested {
            debug!(target: "client", head_height = head.height, "Dropping a block that is too far ahead.");
            return Ok(false);
        }
        let tail = self.chain.tail()?;
        if block.header().height() < tail {
            debug!(target: "client", tail_height = tail, "Dropping a block that is too far behind.");
            return Ok(false);
        }
        // drop the block if a) it is not requested, b) we already processed this height,
        //est-utils/actix-test-utils/src/lib.rs c) it is not building on top of current head
        if !was_requested
            && block.header().prev_hash()
                != &self
                    .chain
                    .head()
                    .map_or_else(|_| CryptoHash::default(), |tip| tip.last_block_hash)
        {
            if self.chain.is_height_processed(block.header().height())? {
                debug!(target: "client", height = block.header().height(), "Dropping a block because we've seen this height before and we didn't request it");
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// Verify the block and rebroadcast it if it is valid, ban the peer if it's invalid.
    /// Ignore all other errors because the full block will be processed later.
    /// Note that this happens before the full block processing logic because we want blocks to be
    /// propagated in the network fast.
    fn verify_and_rebroadcast_block(
        &mut self,
        block: &MaybeValidated<Block>,
        was_requested: bool,
        peer_id: &PeerId,
    ) -> Result<(), near_chain::Error> {
        let res = self.chain.process_block_header(block.header(), &mut vec![]);
        let res = res.and_then(|_| self.chain.validate_block(block));
        match res {
            Ok(_) => {
                let head = self.chain.head()?;
                // do not broadcast blocks that are too far back.
                if (head.height < block.header().height()
                    || &head.epoch_id == block.header().epoch_id())
                    && !was_requested
                    && !self.sync_handler.sync_status.is_syncing()
                {
                    self.rebroadcast_block(block.as_ref().into_inner());
                }
                Ok(())
            }
            Err(e) if e.is_bad_data() => {
                // We don't ban a peer if the block timestamp is too much in the future since it's possible
                // that a block is considered valid in one machine and invalid in another machine when their
                // clocks are not synced.
                if !matches!(e, near_chain::Error::InvalidBlockFutureTime(_)) {
                    self.ban_peer(peer_id.clone(), ReasonForBan::BadBlockHeader);
                }
                Err(e)
            }
            Err(_) => {
                // We are ignoring all other errors and proceeding with the
                // block.  If it is an orphan (i.e. we haven't processed its
                // previous block) than we will get MissingBlock errors.  In
                // those cases we shouldn't reject the block instead passing
                // it along.  Eventually, it'll get saved as an orphan.
                Ok(())
            }
        }
    }

    /// Check if any block with missing chunks is ready to be processed
    pub fn process_blocks_with_missing_chunks(
        &mut self,
        apply_chunks_done_sender: Option<Sender<ApplyChunksDoneMessage>>,
        signer: &Option<Arc<ValidatorSigner>>,
    ) {
        let _span = debug_span!(target: "client", "process_blocks_with_missing_chunks").entered();
        let me = signer.as_ref().map(|signer| signer.validator_id());
        let mut blocks_processing_artifacts = BlockProcessingArtifact::default();
        self.chain.check_blocks_with_missing_chunks(
            &me.map(|x| x.clone()),
            &mut blocks_processing_artifacts,
            apply_chunks_done_sender,
        );
        self.process_block_processing_artifact(blocks_processing_artifacts, signer);
    }

    /// Check if any block in the pending pool is ready to be processed
    pub fn process_pending_blocks(
        &mut self,
        apply_chunks_done_sender: Option<Sender<ApplyChunksDoneMessage>>,
        signer: &Option<Arc<ValidatorSigner>>,
    ) {
        let _span = debug_span!(target: "client", "process_pending_blocks").entered();
        let me = signer.as_ref().map(|signer| signer.validator_id());
        let mut blocks_processing_artifacts = BlockProcessingArtifact::default();
        self.chain.check_pending_blocks(
            &me.map(|x| x.clone()),
            &mut blocks_processing_artifacts,
            apply_chunks_done_sender,
        );
        self.process_block_processing_artifact(blocks_processing_artifacts, signer);
    }
}
