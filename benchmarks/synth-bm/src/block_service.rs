use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
    time::Duration,
    time::Instant,
};

use near_jsonrpc_client::JsonRpcClient;
use near_primitives::{hash::CryptoHash, views::BlockView};
use rand;
use tokio::time;

use crate::rpc::get_latest_block;

pub struct BlockService {
    rpc_client: JsonRpcClient,
    refresh_interval: Duration,
    /// A block that's refreshed every `refresh_interval`.
    block: RwLock<BlockView>,
}

impl BlockService {
    /// Construction must be followed by call to `Self::start`.
    ///
    /// # Panics
    ///
    /// Panics if getting a new block fails.
    pub async fn new(rpc_client: JsonRpcClient) -> Self {
        // Getting a new block hash is relatively cheap, hence just do it every 30 seconds even
        // if longer refresh intervals might be fine too. A shorter interval reduces the chances
        // expiring transactions.
        let refresh_interval = Duration::from_secs(30);
        let block = get_latest_block(&rpc_client).await.expect("should be able to get a block");
        Self { rpc_client, refresh_interval, block: RwLock::new(block) }
    }

    /// # Panics
    ///
    /// Panics if getting a new block fails.
    pub async fn start(self: Arc<Self>) {
        let mut interval = time::interval(self.refresh_interval);

        tokio::spawn(async move {
            // First tick returns immediately, but block was just fetched in `Self::new`,
            // hence skip one tick.
            interval.tick().await;
            loop {
                interval.tick().await;
                let new_block = get_latest_block(&self.rpc_client)
                    .await
                    .expect("should be able to get a block");
                let mut block = self.block.write().unwrap();
                *block = new_block;
            }
        });
    }

    pub fn get_block(&self) -> BlockView {
        self.block.read().unwrap().clone()
    }

    pub fn get_block_hash(&self) -> CryptoHash {
        self.get_block().header.hash
    }
}

pub struct ShardAwareRpcClient {
    // Primary client
    primary_client: JsonRpcClient,
    // Cache of shard layout and tracking nodes
    shard_layout_cache: Option<ShardLayoutCache>,
    // Block service for getting latest block info
    block_service: Arc<BlockService>,
}

struct ShardLayoutCache {
    shard_layout_view: ShardLayoutView,
    timestamp: Instant,
}

impl ShardAwareRpcClient {
    pub async fn new(primary_rpc_url: &str) -> Result<Self, String> {
        let primary_client = JsonRpcClient::connect(primary_rpc_url);
        let block_service = Arc::new(BlockService::new(primary_client.clone()).await);

        // Start the block service
        block_service.clone().start().await;

        let mut client = Self { primary_client, shard_layout_cache: None, block_service };

        // Initialize the cache
        client.refresh_shard_layout().await?;

        Ok(client)
    }

    async fn refresh_shard_layout(&mut self) -> Result<(), String> {
        let request = QueryRequest::ViewShardLayout {};

        // Create RPC query request
        let rpc_query_request =
            RpcQueryRequest { block_reference: BlockReference::latest(), request };

        // Send the query
        let response = self
            .primary_client
            .query(rpc_query_request)
            .await
            .map_err(|e| format!("Failed to query shard layout: {}", e))?;

        if let QueryResponseKind::ViewShardLayout(layout_view) = response.kind {
            self.shard_layout_cache = Some(ShardLayoutCache {
                shard_layout_view: layout_view,
                timestamp: Instant::now(),
            });

            Ok(())
        } else {
            Err("Unexpected response type".to_string())
        }
    }

    async fn check_and_refresh_cache(&mut self) -> Result<(), String> {
        // Get the latest block height
        let current_block = self.block_service.get_block();
        let current_height = current_block.header.height;

        // Check if we need to refresh the cache
        let needs_refresh = match &self.shard_layout_cache {
            None => true,
            Some(cache) => {
                // Refresh if we've reached the epoch end height
                if let Some(end_height) = cache.shard_layout_view.epoch_end_height {
                    if current_height >= end_height { true } else { false }
                } else {
                    // If we don't know the epoch end height, refresh after a certain time
                    // This is a fallback mechanism
                    cache.timestamp.elapsed() > Duration::from_secs(600)
                }
            }
        };

        if needs_refresh {
            self.refresh_shard_layout().await?;
        }

        Ok(())
    }

    async fn get_client_for_account(
        &mut self,
        account_id: &AccountId,
    ) -> Result<JsonRpcClient, String> {
        // Check and refresh cache if needed
        self.check_and_refresh_cache().await?;

        let cache = self
            .shard_layout_cache
            .as_ref()
            .ok_or_else(|| "Shard layout cache not initialized".to_string())?;

        // Determine which shard this account belongs to using the ShardLayout's method
        let shard_id = cache.shard_layout_view.shard_layout.account_id_to_shard_id(account_id);

        // Find a node that tracks this shard
        if let Some(node_infos) = cache.shard_layout_view.tracking_shards.get(&shard_id) {
            if !node_infos.is_empty() {
                // Pick a random node from the list for load balancing
                let node_info = &node_infos[rand::thread_rng().gen_range(0..node_infos.len())];
                return Ok(JsonRpcClient::connect(&node_info.url));
            }
        }

        // Fall back to primary client if no specific node found
        Ok(self.primary_client.clone())
    }

    // Example method to send a transaction with automatic routing
    pub async fn send_transaction(
        &mut self,
        tx: SignedTransaction,
    ) -> Result<RpcTransactionResponse, String> {
        let signer_account_id = tx.transaction.signer_id.clone();
        let client = self.get_client_for_account(&signer_account_id).await?;

        let request = RpcSendTransactionRequest {
            signed_transaction: tx,
            wait_until: TxExecutionStatus::None,
        };

        client.tx(request).await.map_err(|e| e.to_string())
    }

    // Get transaction status with automatic routing
    pub async fn get_transaction_status(
        &mut self,
        tx_hash: CryptoHash,
        signer_account_id: &AccountId,
    ) -> Result<RpcTransactionResponse, String> {
        let client = self.get_client_for_account(signer_account_id).await?;

        let request = RpcTransactionStatusRequest {
            transaction_info: TransactionInfo::TransactionId {
                hash: tx_hash,
                account_id: signer_account_id.clone(),
            },
            wait_until: TxExecutionStatus::None,
        };

        client.tx(request).await.map_err(|e| e.to_string())
    }

    // Get current shard layout information
    pub fn get_current_shard_layout(&self) -> Option<&ShardLayout> {
        self.shard_layout_cache.as_ref().map(|cache| &cache.shard_layout_view.shard_layout)
    }

    // Get current epoch information
    pub fn get_current_epoch_info(
        &self,
    ) -> Option<(EpochId, u64, BlockHeight, Option<BlockHeight>)> {
        self.shard_layout_cache.as_ref().map(|cache| {
            let view = &cache.shard_layout_view;
            (
                view.epoch_id.clone(),
                view.epoch_height,
                view.epoch_start_height,
                view.epoch_end_height,
            )
        })
    }
}
