use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use clap::Parser;
use near_async::time::Clock;
use near_chain::runtime::NightshadeRuntime;
use near_chain::stateless_validation::state_witness::validate_prepared_transactions;
use near_chain::types::{RuntimeStorageConfig, StorageDataSource};
use near_chain::{Chain, ChainGenesis, ChainStore, DoomslugThresholdMode};
use near_epoch_manager::shard_tracker::{ShardTracker, TrackedConfig};
use near_epoch_manager::EpochManager;
use near_primitives::stateless_validation::ChunkStateWitness;
use near_primitives::types::EpochId;
use near_store::Store;
use nearcore::NearConfig;
use nearcore::NightshadeRuntimeExt;

#[derive(clap::Subcommand)]
pub enum StateWitnessCmd {
    /// Creates state witness for given height and shard.
    Create(CreateWitnessCmd),
    /// Prints latest state witnesses saved to DB.
    Latest(LatestWitnessesCmd),
    /// Validates given state witness and hangs.
    Validate(ValidateWitnessCmd),
}

impl StateWitnessCmd {
    pub(crate) fn run(&self, home_dir: &Path, near_config: NearConfig, store: Store) {
        match self {
            StateWitnessCmd::Create(cmd) => cmd.run(home_dir, near_config, store),
            StateWitnessCmd::Latest(cmd) => cmd.run(near_config, store),
            StateWitnessCmd::Validate(cmd) => cmd.run(home_dir, near_config, store),
        }
    }
}

#[derive(Parser)]
pub struct CreateWitnessCmd {
    /// Block height
    #[arg(long)]
    height: u64,
    /// Shard id
    #[arg(long)]
    shard_id: u64,
}

impl CreateWitnessCmd {
    pub(crate) fn run(&self, home_dir: &Path, near_config: NearConfig, store: Store) {
        let chain_genesis = ChainGenesis::new(&near_config.genesis.config);
        let epoch_manager =
            EpochManager::new_arc_handle(store.clone(), &near_config.genesis.config);
        let runtime_adapter =
            NightshadeRuntime::from_config(home_dir, store, &near_config, epoch_manager.clone())
                .expect("could not create the transaction runtime");
        let shard_tracker = ShardTracker::new(
            TrackedConfig::from_config(&near_config.client_config),
            epoch_manager.clone(),
        );

        // TODO(stateless_validation): consider using `ChainStore` instead of
        // `Chain`.
        let mut chain = Chain::new_for_view_client(
            Clock::real(),
            epoch_manager.clone(),
            shard_tracker,
            runtime_adapter.clone(),
            &chain_genesis,
            DoomslugThresholdMode::TwoThirds,
            false,
        )
        .unwrap();

        let block = chain.get_block_by_height(self.height).unwrap();
        let prev_block = chain.get_block(block.header().prev_hash()).unwrap();
        let chunk_header =
            block.chunks().iter().find(|chunk| chunk.shard_id() == self.shard_id).unwrap().clone();
        let chunk = chain.get_chunk_clone_from_header(&chunk_header).unwrap();
        let prev_chunk_header = prev_block
            .chunks()
            .iter()
            .find(|chunk| chunk.shard_id() == self.shard_id)
            .unwrap()
            .clone();

        let chunk_header = chunk.cloned_header();
        let transactions_validation_storage_config = RuntimeStorageConfig {
            state_root: chunk_header.prev_state_root(),
            use_flat_storage: true,
            source: StorageDataSource::Db,
            state_patch: Default::default(),
        };

        // We call `validate_prepared_transactions()` here because we need storage proof for transactions validation.
        // Normally it is provided by chunk producer, but for shadow validation we need to generate it ourselves.
        let Ok(validated_transactions) = validate_prepared_transactions(
            &chain,
            runtime_adapter.as_ref(),
            &chunk_header,
            transactions_validation_storage_config,
            chunk.transactions(),
        ) else {
            panic!("Could not produce storage proof for new transactions");
        };

        chain
            .create_and_save_state_witness(
                // Doesn't matter if used for testing.
                "alice.near".parse().unwrap(),
                prev_block.header(),
                &prev_chunk_header,
                &chunk,
                validated_transactions.storage_proof,
                true,
            )
            .unwrap();
    }
}

#[derive(Parser)]
pub struct LatestWitnessesCmd {
    /// Block height
    #[arg(long)]
    height: Option<u64>,

    /// Shard id
    #[arg(long)]
    shard_id: Option<u64>,

    /// Epoch Id
    #[arg(long)]
    epoch_id: Option<EpochId>,

    /// Pretty-print using the "{:#?}" formatting.
    #[arg(long)]
    pretty: bool,

    /// Print the raw &[u8], can be pasted into rust code
    #[arg(long)]
    binary: bool,
}

impl LatestWitnessesCmd {
    pub(crate) fn run(&self, near_config: NearConfig, store: Store) {
        let chain_store =
            Rc::new(ChainStore::new(store, near_config.genesis.config.genesis_height, false));

        let witnesses = chain_store
            .get_latest_witnesses(self.height, self.shard_id, self.epoch_id.clone())
            .unwrap();
        println!("Found {} witnesses:", witnesses.len());
        for (i, witness) in witnesses.iter().enumerate() {
            println!(
                "#{} (height: {}, shard_id: {}, epoch_id: {:?}):",
                i,
                witness.chunk_header.height_created(),
                witness.chunk_header.shard_id(),
                witness.epoch_id
            );
            if self.pretty {
                println!("{:#?}", witness);
            } else if self.binary {
                println!("{:?}", borsh::to_vec(witness).unwrap());
            } else {
                println!("{:?}", witness);
            }
            println!("");
        }
    }
}

#[derive(Parser)]
pub struct ValidateWitnessCmd {
    /// File with state witness saved as vector in JSON.
    #[arg(long)]
    input_file: PathBuf,
}

impl ValidateWitnessCmd {
    pub(crate) fn run(&self, home_dir: &Path, near_config: NearConfig, store: Store) {
        let encoded_witness: Vec<u8> =
            serde_json::from_reader(BufReader::new(std::fs::File::open(&self.input_file).unwrap()))
                .unwrap();
        let witness: ChunkStateWitness = borsh::BorshDeserialize::try_from_slice(&encoded_witness)
            .expect("Failed to deserialize witness");
        let chain_genesis = ChainGenesis::new(&near_config.genesis.config);
        let epoch_manager =
            EpochManager::new_arc_handle(store.clone(), &near_config.genesis.config);
        let runtime_adapter =
            NightshadeRuntime::from_config(home_dir, store, &near_config, epoch_manager.clone())
                .expect("could not create the transaction runtime");
        let shard_tracker = ShardTracker::new(
            TrackedConfig::from_config(&near_config.client_config),
            epoch_manager.clone(),
        );
        // TODO(stateless_validation): consider using `ChainStore` instead of
        // `Chain`.
        let chain = Chain::new_for_view_client(
            Clock::real(),
            epoch_manager.clone(),
            shard_tracker,
            runtime_adapter.clone(),
            &chain_genesis,
            DoomslugThresholdMode::TwoThirds,
            false,
        )
        .unwrap();
        chain
            .shadow_validate_state_witness(
                witness,
                epoch_manager.as_ref(),
                runtime_adapter.as_ref(),
            )
            .unwrap();
        // Not optimal, but needed to wait for potential `validate_state_witness` failure.
        loop {
            std::thread::sleep(core::time::Duration::from_secs(60));
        }
    }
}
