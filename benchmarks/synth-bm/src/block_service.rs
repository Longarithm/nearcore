use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, RwLock},
    time::Duration,
    time::Instant,
};

use near_jsonrpc_client::JsonRpcClient;
use near_jsonrpc_primitives::types::transactions::{
    RpcSendTransactionRequest, RpcTransactionResponse,
};
use near_primitives::hash::CryptoHash;
use near_primitives::transaction::SignedTransaction;
use near_primitives::types::{AccountId, BlockHeight, EpochHeight, EpochId, ShardId};
use near_primitives::views::{BlockView, TxExecutionStatus};
use rand::Rng;
use serde_json;
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
    // Cache of JsonRpcClients for different nodes
    client_cache: HashMap<String, JsonRpcClient>,
}

#[derive(Clone)]
struct ShardLayoutCache {
    shard_layout_view: ShardTrackersView,
    timestamp: Instant,
}

// NodeInfo structure
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct NodeInfo {
    pub url: String,
    pub account_id: Option<AccountId>,
    pub is_validator: bool,
    pub is_archival: bool,
}

// ShardTrackersView structure
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct ShardTrackersView {
    pub shard_ids: Vec<ShardId>,
    pub boundary_accounts: Vec<AccountId>,
    pub tracking_shards: HashMap<ShardId, Vec<NodeInfo>>,
    pub epoch_id: EpochId,
    pub epoch_height: EpochHeight,
    pub epoch_start_height: BlockHeight,
    pub epoch_end_height: Option<BlockHeight>,
}

pub fn account_id_to_shard_id(
    shard_ids: &Vec<ShardId>,
    boundary_accounts: &Vec<AccountId>,
    account_id: &AccountId,
) -> ShardId {
    let mut shard_id_index = 0;
    for boundary_account in boundary_accounts {
        if account_id < boundary_account {
            break;
        }
        shard_id_index += 1;
    }
    shard_ids[shard_id_index]
}

impl ShardAwareRpcClient {
    pub async fn new(primary_rpc_url: &str) -> Result<Self, String> {
        let primary_client = JsonRpcClient::connect(primary_rpc_url);
        let block_service = Arc::new(BlockService::new(primary_client.clone()).await);

        // Start the block service
        block_service.clone().start().await;

        let mut client = Self {
            primary_client,
            shard_layout_cache: None,
            block_service,
            client_cache: HashMap::new(),
        };

        // Initialize the cache
        client.refresh_shard_layout().await?;

        Ok(client)
    }

    async fn refresh_shard_layout(&mut self) -> Result<(), String> {
        // Define the RPC endpoint URL (using the same URL as the primary client)
        let rpc_url = self.primary_client.server_addr().to_string();

        // Create the JSON-RPC request payload
        let request_payload = r#"{
            "jsonrpc": "2.0",
            "id": "dontcare",
            "method": "EXPERIMENTAL_shard_trackers",
            "params": {}
        }"#;

        // Execute the curl command using tokio::process::Command for async execution
        let output = tokio::process::Command::new("curl")
            .arg("-s")
            .arg("-X")
            .arg("POST")
            .arg("-H")
            .arg("Content-Type: application/json")
            .arg("-d")
            .arg(request_payload)
            .arg(rpc_url)
            .output()
            .await
            .map_err(|e| format!("curl command failed: {}", e))?;

        if !output.status.success() {
            return Err(format!(
                "curl command failed: {}",
                String::from_utf8_lossy(&output.stderr)
            ));
        }

        // Get the response as a string
        let response_str = String::from_utf8(output.stdout)
            .map_err(|e| format!("Failed to parse curl output: {}", e))?;

        // Define a simple struct to deserialize the JSON-RPC response
        #[derive(serde::Deserialize)]
        struct JsonRpcResponse {
            result: ShardTrackersView,
        }

        // Deserialize the JSON response
        let response: JsonRpcResponse = serde_json::from_str(&response_str)
            .map_err(|e| format!("Failed to deserialize response: {}", e))?;

        // Update the cache with the deserialized ShardTrackersView
        self.shard_layout_cache = Some(ShardLayoutCache {
            shard_layout_view: response.result,
            timestamp: std::time::Instant::now(),
        });

        Ok(())
    }

    async fn check_and_refresh_cache(&mut self) -> Result<(), String> {
        // Get the latest block height
        let current_block = self.block_service.get_block();
        let current_height = current_block.header.height;

        // Check if we need to refresh the cache
        let needs_refresh = match &self.shard_layout_cache {
            None => true,
            Some(cache) => {
                // Check if all shards are covered by validators
                let all_shards_covered =
                    self.are_all_shards_covered_by_validators(&cache.shard_layout_view);

                // Refresh if we've reached the estimated epoch end height
                if let Some(end_height) = cache.shard_layout_view.epoch_end_height {
                    if current_height >= end_height {
                        true
                    } else if !all_shards_covered {
                        // If not all shards are covered by validators, refresh more frequently
                        cache.timestamp.elapsed() > Duration::from_secs(30)
                    } else {
                        false
                    }
                } else {
                    // If we don't have an estimated end height, refresh after a certain time
                    // or more frequently if not all shards are covered
                    if !all_shards_covered {
                        cache.timestamp.elapsed() > Duration::from_secs(30)
                    } else {
                        cache.timestamp.elapsed() > Duration::from_secs(600)
                    }
                }
            }
        };

        if needs_refresh {
            self.refresh_shard_layout().await?;
        }

        Ok(())
    }

    // New method to check if all shards are covered by validators
    fn are_all_shards_covered_by_validators(&self, shard_layout_view: &ShardTrackersView) -> bool {
        // Get the total number of shards from the shard layout
        let num_shards = shard_layout_view.shard_ids.len();

        // Create a set to track which shards are covered by validators
        let mut covered_shards = HashSet::new();

        // Check each shard in the tracking_shards map
        for (shard_id, node_infos) in &shard_layout_view.tracking_shards {
            // Check if any of the nodes tracking this shard is a validator
            let has_validator = node_infos.iter().any(|node_info| {
                // A node is considered a validator if it has an account_id
                // This is a simplification - in a real implementation, you might want to check
                // if the account is actually in the current validator set
                node_info.account_id.is_some()
            });

            if has_validator {
                covered_shards.insert(*shard_id);
            }
        }

        // Check if all shards are covered
        covered_shards.len() == num_shards
    }

    // Modify get_client_for_account to prefer validators and use client cache
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
        let shard_id = account_id_to_shard_id(
            &cache.shard_layout_view.shard_ids,
            &cache.shard_layout_view.boundary_accounts,
            account_id,
        );

        // Find a node that tracks this shard
        if let Some(node_infos) = cache.shard_layout_view.tracking_shards.get(&shard_id) {
            if !node_infos.is_empty() {
                // First, try to find validators tracking this shard
                let validators: Vec<&NodeInfo> =
                    node_infos.iter().filter(|node_info| node_info.account_id.is_some()).collect();

                if !validators.is_empty() {
                    // Pick a random validator from the list for load balancing
                    let validator = &validators[rand::thread_rng().gen_range(0..validators.len())];

                    // Check if we already have a client for this URL in the cache
                    if let Some(client) = self.client_cache.get(&validator.url) {
                        return Ok(client.clone());
                    }

                    // Create a new client and add it to the cache
                    let client = JsonRpcClient::connect(&validator.url);
                    self.client_cache.insert(validator.url.clone(), client.clone());
                    return Ok(client);
                }

                // If no validators found, fall back to any node
                let node_info = &node_infos[rand::thread_rng().gen_range(0..node_infos.len())];

                // Check if we already have a client for this URL in the cache
                if let Some(client) = self.client_cache.get(&node_info.url) {
                    return Ok(client.clone());
                }

                // Create a new client and add it to the cache
                let client = JsonRpcClient::connect(&node_info.url);
                self.client_cache.insert(node_info.url.clone(), client.clone());
                return Ok(client);
            }
        }

        // Fall back to primary client if no specific node found
        Ok(self.primary_client.clone())
    }

    // Add a method to check if all shards are covered
    pub fn are_all_shards_covered(&self) -> bool {
        match &self.shard_layout_cache {
            None => false,
            Some(cache) => self.are_all_shards_covered_by_validators(&cache.shard_layout_view),
        }
    }

    // Add a method to get uncovered shards
    pub fn get_uncovered_shards(&self) -> Vec<ShardId> {
        match &self.shard_layout_cache {
            None => vec![],
            Some(cache) => {
                let view = &cache.shard_layout_view;
                let num_shards = view.shard_ids.len();

                let mut uncovered_shards = Vec::new();

                for shard_id in 0..num_shards {
                    if let Some(node_infos) = view.tracking_shards.get(&(shard_id as u64)) {
                        // Check if any of the nodes tracking this shard is a validator
                        let has_validator =
                            node_infos.iter().any(|node_info| node_info.account_id.is_some());

                        if !has_validator {
                            uncovered_shards.push(shard_id as u64);
                        }
                    } else {
                        // If no nodes are tracking this shard at all
                        uncovered_shards.push(shard_id as u64);
                    }
                }

                uncovered_shards
            }
        }
    }

    // Example method to send a transaction with automatic routing
    pub async fn send_transaction(
        &mut self,
        tx: SignedTransaction,
    ) -> Result<RpcTransactionResponse, String> {
        let signer_account_id = tx.transaction.signer_id().clone();
        let client = self.get_client_for_account(&signer_account_id).await?;

        let request = RpcSendTransactionRequest {
            signed_transaction: tx,
            wait_until: TxExecutionStatus::None,
        };

        client.call(request).await.map_err(|e| e.to_string())
    }

    // Add a method to proactively create clients for all validators
    pub async fn preload_validator_clients(&mut self) -> Result<(), String> {
        // Check and refresh cache if needed
        self.check_and_refresh_cache().await?;

        let cache = self
            .shard_layout_cache
            .as_ref()
            .ok_or_else(|| "Shard layout cache not initialized".to_string())?;

        // Iterate through all shards and their tracking nodes
        for (_, node_infos) in &cache.shard_layout_view.tracking_shards {
            for node_info in node_infos {
                // Only preload clients for validators
                if node_info.account_id.is_some() && !self.client_cache.contains_key(&node_info.url)
                {
                    let client = JsonRpcClient::connect(&node_info.url);
                    self.client_cache.insert(node_info.url.clone(), client);
                }
            }
        }

        Ok(())
    }

    // Add a method to clear stale clients from the cache
    pub fn clear_stale_clients(&mut self) -> Result<(), String> {
        if let Some(cache) = &self.shard_layout_cache {
            // Collect all current valid URLs
            let mut valid_urls = HashSet::new();

            // Add primary client URL
            valid_urls.insert(self.primary_client.server_addr().to_string());

            // Add all URLs from the current shard layout
            for (_, node_infos) in &cache.shard_layout_view.tracking_shards {
                for node_info in node_infos {
                    valid_urls.insert(node_info.url.clone());
                }
            }

            // Remove clients that are no longer in the valid set
            self.client_cache.retain(|url, _| valid_urls.contains(url));

            Ok(())
        } else {
            Err("Shard layout cache not initialized".to_string())
        }
    }
}
