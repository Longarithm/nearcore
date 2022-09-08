#[cfg(feature = "protocol_feature_flat_state")]
mod imp {
    use crate::{ChainStore, ChainStoreAccess};
    use near_primitives::hash::CryptoHash;
    use near_store::flat_state::FlatState;
    use std::sync::{Arc, RwLock};

    #[derive(Default)]
    pub struct ChainFlatState {
        pub lock: Arc<RwLock<()>>,
    }

    impl ChainFlatState {
        pub fn create_flat_state(
            &self,
            prev_block_hash: &CryptoHash,
            chain_store: &ChainStore,
        ) -> Option<FlatState> {
            near_store::flat_state::maybe_new(true, chain_store.store(), prev_block_hash, lock)
        }
    }
}

#[cfg(not(feature = "protocol_feature_flat_state"))]
mod imp {
    #[derive(Default)]
    pub enum ChainFlatState {}

    impl ChainFlatState {
        pub fn create_flat_state(
            &self,
            _prev_block_hash: &CryptoHash,
            _chain_store: &ChainStore,
        ) -> Option<FlatState> {
            near_store::flat_state::maybe_new(false, chain_store.store())
        }
    }
}

pub use imp::ChainFlatState;
