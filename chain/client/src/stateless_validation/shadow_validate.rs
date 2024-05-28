use near_chain::types::StorageDataSource;
use near_chain::{Block, BlockHeader};
use near_chain_primitives::Error;
use near_primitives::sharding::{ShardChunk, ShardChunkHeader};

use crate::Client;

impl Client {
    // Temporary feature to make node produce state witness for every chunk in every processed block
    // and then self-validate it.
    pub(crate) fn shadow_validate_block_chunks(&mut self, block: &Block) -> Result<(), Error> {
        if !cfg!(feature = "shadow_chunk_validation") {
            return Ok(());
        }
        let block_hash = block.hash();
        tracing::debug!(target: "client", ?block_hash, "shadow validation for block chunks");
        let prev_block = self.chain.get_block(block.header().prev_hash())?;
        let prev_block_chunks = prev_block.chunks();
        for chunk in
            block.chunks().iter().filter(|chunk| chunk.is_new_chunk(block.header().height()))
        {
            let chunk = self.chain.get_chunk_clone_from_header(chunk)?;
            let prev_chunk_header = prev_block_chunks.get(chunk.shard_id() as usize).unwrap();
            if let Err(err) =
                self.shadow_validate_chunk(prev_block.header(), prev_chunk_header, &chunk)
            {
                near_chain::metrics::SHADOW_CHUNK_VALIDATION_FAILED_TOTAL.inc();
                tracing::error!(
                    target: "client",
                    ?err,
                    shard_id = chunk.shard_id(),
                    ?block_hash,
                    "shadow chunk validation failed"
                );
            }
        }
        Ok(())
    }

    fn shadow_validate_chunk(
        &mut self,
        prev_block_header: &BlockHeader,
        prev_chunk_header: &ShardChunkHeader,
        chunk: &ShardChunk,
    ) -> Result<(), Error> {
        let witness = self.chain.create_and_save_state_witness(
            // Setting arbitrary chunk producer is OK for shadow validation
            "alice.near".parse().unwrap(),
            prev_block_header,
            prev_chunk_header,
            chunk,
            self.runtime_adapter.as_ref(),
            self.config.save_latest_witnesses,
        )?;
        self.chain.shadow_validate_state_witness(
            witness,
            self.epoch_manager.as_ref(),
            self.runtime_adapter.as_ref(),
        )?;
        Ok(())
    }
}
