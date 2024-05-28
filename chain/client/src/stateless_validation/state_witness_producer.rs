use near_async::messaging::{CanSend, IntoSender};
use near_chain::BlockHeader;
use near_chain_primitives::Error;
use near_primitives::challenge::PartialState;
use near_primitives::checked_feature;
use near_primitives::sharding::{ShardChunk, ShardChunkHeader};
use near_primitives::stateless_validation::StoredChunkStateTransitionData;
use near_primitives::types::EpochId;

use crate::stateless_validation::chunk_validator::send_chunk_endorsement_to_block_producers;
use crate::Client;

use super::partial_witness::partial_witness_actor::DistributeStateWitnessRequest;

impl Client {
    /// Distributes the chunk state witness to chunk validators that are
    /// selected to validate this chunk.
    pub fn send_chunk_state_witness_to_chunk_validators(
        &mut self,
        epoch_id: &EpochId,
        prev_block_header: &BlockHeader,
        prev_chunk_header: &ShardChunkHeader,
        chunk: &ShardChunk,
        transactions_storage_proof: Option<PartialState>,
    ) -> Result<(), Error> {
        let protocol_version = self.epoch_manager.get_epoch_protocol_version(epoch_id)?;
        if !checked_feature!("stable", StatelessValidationV0, protocol_version) {
            return Ok(());
        }
        let chunk_header = chunk.cloned_header();
        let shard_id = chunk_header.shard_id();
        let _span = tracing::debug_span!(target: "client", "send_chunk_state_witness", chunk_hash=?chunk_header.chunk_hash(), ?shard_id).entered();

        let my_signer = self.validator_signer.as_ref().ok_or(Error::NotAValidator)?.clone();
        let state_witness = self.chain.create_and_save_state_witness(
            my_signer.validator_id().clone(),
            prev_block_header,
            prev_chunk_header,
            chunk,
            transactions_storage_proof,
            self.config.save_latest_witnesses,
        )?;

        if self.config.save_latest_witnesses {
            self.chain.chain_store.save_latest_chunk_state_witness(&state_witness)?;
        }

        let height = chunk_header.height_created();
        if self
            .epoch_manager
            .get_chunk_validator_assignments(epoch_id, shard_id, height)?
            .contains(my_signer.validator_id())
        {
            // Bypass state witness validation if we created state witness. Endorse the chunk immediately.
            tracing::debug!(target: "client", chunk_hash=?chunk_header.chunk_hash(), "send_chunk_endorsement_from_chunk_producer");
            send_chunk_endorsement_to_block_producers(
                &chunk_header,
                self.epoch_manager.as_ref(),
                my_signer.as_ref(),
                &self.network_adapter.clone().into_sender(),
                self.chunk_endorsement_tracker.as_ref(),
            );
        }

        self.partial_witness_adapter.send(DistributeStateWitnessRequest {
            epoch_id: epoch_id.clone(),
            chunk_header,
            state_witness,
        });
        Ok(())
    }
}
