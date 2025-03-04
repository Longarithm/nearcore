use std::{
    collections::{HashMap, HashSet},
    sync::atomic::{AtomicBool, Ordering},
    sync::{Arc, RwLock as StdRwLock},
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
use tokio::sync::RwLock;
use tokio::time;

use crate::rpc::get_latest_block;

pub struct BlockService {
    rpc_client: JsonRpcClient,
    refresh_interval: Duration,
    /// A block that's refreshed every `refresh_interval`.
    block: StdRwLock<BlockView>,
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
        Self { rpc_client, refresh_interval, block: StdRwLock::new(block) }
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
    shard_layout_cache: Arc<RwLock<Option<ShardLayoutCache>>>,
    // Block service for getting latest block info
    block_service: Arc<BlockService>,
    // Cache of JsonRpcClients for different nodes
    client_cache: Arc<RwLock<HashMap<String, JsonRpcClient>>>,
    // Flag to control the cache update task
    cache_updater_running: Arc<AtomicBool>,
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

// Implement Drop for ShardAwareRpcClient
impl Drop for ShardAwareRpcClient {
    fn drop(&mut self) {
        self.cache_updater_running.store(false, Ordering::Relaxed);
    }
}

impl ShardAwareRpcClient {
    pub async fn new(primary_rpc_url: &str) -> Result<Self, String> {
        let primary_client = JsonRpcClient::connect(primary_rpc_url);
        let block_service = Arc::new(BlockService::new(primary_client.clone()).await);

        // Start the block service
        block_service.clone().start().await;

        let shard_layout_cache = Arc::new(RwLock::new(None));
        let client_cache = Arc::new(RwLock::new(HashMap::new()));
        let cache_updater_running = Arc::new(AtomicBool::new(true));

        let client = Self {
            primary_client,
            shard_layout_cache,
            block_service,
            client_cache,
            cache_updater_running,
        };

        // Initialize the cache
        client.refresh_shard_layout().await?;

        // Start the cache update task
        client.start_cache_updater();

        Ok(client)
    }

    // Start a background task to periodically update the cache
    fn start_cache_updater(&self) {
        let shard_layout_cache = self.shard_layout_cache.clone();
        let block_service = self.block_service.clone();
        let client_cache = self.client_cache.clone();
        let primary_client = self.primary_client.clone();
        let running = self.cache_updater_running.clone();

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(30));

            while running.load(Ordering::Relaxed) {
                interval.tick().await;

                // Check if we need to refresh the cache
                let needs_refresh = {
                    let cache_read = shard_layout_cache.read().await;
                    match &*cache_read {
                        None => true,
                        Some(cache) => {
                            // Check if all shards are covered by validators
                            let all_shards_covered = {
                                // Check each shard in the tracking_shards map
                                let num_shards = cache.shard_layout_view.shard_ids.len();
                                let mut covered_shards = HashSet::new();

                                for (shard_id, node_infos) in
                                    &cache.shard_layout_view.tracking_shards
                                {
                                    // Check if any of the nodes tracking this shard is a validator
                                    let has_validator = node_infos
                                        .iter()
                                        .any(|node_info| node_info.account_id.is_some());

                                    if has_validator {
                                        covered_shards.insert(*shard_id);
                                    }
                                }

                                covered_shards.len() == num_shards
                            };

                            // Get the latest block height
                            let current_block = block_service.get_block();
                            let current_height = current_block.header.height;

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
                    }
                };

                if needs_refresh {
                    // Refresh the shard layout
                    if let Err(e) = Self::refresh_shard_layout_static(
                        primary_client.clone(),
                        shard_layout_cache.clone(),
                    )
                    .await
                    {
                        eprintln!("Failed to refresh shard layout: {}", e);
                    }

                    // Clear stale clients
                    if let Err(e) = Self::clear_stale_clients_static(
                        primary_client.clone(),
                        shard_layout_cache.clone(),
                        client_cache.clone(),
                    )
                    .await
                    {
                        eprintln!("Failed to clear stale clients: {}", e);
                    }
                }
            }
        });
    }

    // Static version of refresh_shard_layout that doesn't require &mut self
    async fn refresh_shard_layout_static(
        primary_client: JsonRpcClient,
        shard_layout_cache: Arc<RwLock<Option<ShardLayoutCache>>>,
    ) -> Result<(), String> {
        // Define the RPC endpoint URL (using the same URL as the primary client)
        let rpc_url = primary_client.server_addr().to_string();

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

        // Print the shard trackers information
        println!("EXPERIMENTAL_shard_trackers result:");
        println!("  Shard IDs: {:?}", response.result.shard_ids);
        println!("  Epoch ID: {:?}", response.result.epoch_id);
        println!("  Epoch Height: {}", response.result.epoch_height);
        println!("  Epoch Start Height: {}", response.result.epoch_start_height);
        println!("  Epoch End Height: {:?}", response.result.epoch_end_height);

        // Print information about nodes tracking each shard
        println!("Current nodes available for transaction sending:");
        for (shard_id, nodes) in &response.result.tracking_shards {
            println!("  Shard {}: {} nodes", shard_id, nodes.len());
            for (i, node) in nodes.iter().enumerate() {
                println!(
                    "    Node {}: URL={}, Account={:?}, Validator={}, Archival={}",
                    i + 1,
                    node.url,
                    node.account_id,
                    node.is_validator,
                    node.is_archival
                );
            }
        }

        // Update the cache with the deserialized ShardTrackersView
        let mut cache_write = shard_layout_cache.write().await;
        *cache_write = Some(ShardLayoutCache {
            shard_layout_view: response.result,
            timestamp: std::time::Instant::now(),
        });

        Ok(())
    }

    // For backward compatibility, keep the original method but delegate to the static version
    async fn refresh_shard_layout(&self) -> Result<(), String> {
        Self::refresh_shard_layout_static(
            self.primary_client.clone(),
            self.shard_layout_cache.clone(),
        )
        .await
    }

    // Static version of clear_stale_clients that doesn't require &mut self
    async fn clear_stale_clients_static(
        primary_client: JsonRpcClient,
        shard_layout_cache: Arc<RwLock<Option<ShardLayoutCache>>>,
        client_cache: Arc<RwLock<HashMap<String, JsonRpcClient>>>,
    ) -> Result<(), String> {
        let cache_read = shard_layout_cache.read().await;
        if let Some(cache) = &*cache_read {
            // Collect all current valid URLs
            let mut valid_urls = HashSet::new();

            // Add primary client URL
            valid_urls.insert(primary_client.server_addr().to_string());

            // Add all URLs from the current shard layout
            for (_, node_infos) in &cache.shard_layout_view.tracking_shards {
                for node_info in node_infos {
                    valid_urls.insert(node_info.url.clone());
                }
            }

            // Remove clients that are no longer in the valid set
            let mut client_cache_write = client_cache.write().await;
            client_cache_write.retain(|url, _| valid_urls.contains(url));

            Ok(())
        } else {
            Err("Shard layout cache not initialized".to_string())
        }
    }

    // Get client for account without requiring &mut self
    async fn get_client_for_account(
        &self,
        account_id: &AccountId,
    ) -> Result<JsonRpcClient, String> {
        // Read from the cache
        let cache_read = self.shard_layout_cache.read().await;
        let cache =
            cache_read.as_ref().ok_or_else(|| "Shard layout cache not initialized".to_string())?;

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
                    {
                        let client_cache_read = self.client_cache.read().await;
                        if let Some(client) = client_cache_read.get(&validator.url) {
                            return Ok(client.clone());
                        }
                    }

                    // Create a new client and add it to the cache
                    let client = JsonRpcClient::connect(&validator.url);
                    {
                        let mut client_cache_write = self.client_cache.write().await;
                        client_cache_write.insert(validator.url.clone(), client.clone());
                    }
                    return Ok(client);
                }

                // If no validators found, fall back to any node
                let node_info = &node_infos[rand::thread_rng().gen_range(0..node_infos.len())];

                // Check if we already have a client for this URL in the cache
                {
                    let client_cache_read = self.client_cache.read().await;
                    if let Some(client) = client_cache_read.get(&node_info.url) {
                        return Ok(client.clone());
                    }
                }

                // Create a new client and add it to the cache
                let client = JsonRpcClient::connect(&node_info.url);
                {
                    let mut client_cache_write = self.client_cache.write().await;
                    client_cache_write.insert(node_info.url.clone(), client.clone());
                }
                return Ok(client);
            }
        }

        // If no suitable node found, fall back to the primary client
        Ok(self.primary_client.clone())
    }

    // Updated send_transaction to take &self instead of &mut self
    pub async fn send_transaction(
        &self,
        tx: SignedTransaction,
        wait_until: TxExecutionStatus,
    ) -> Result<RpcTransactionResponse, String> {
        let signer_account_id = tx.transaction.signer_id().clone();
        let client = self.get_client_for_account(&signer_account_id).await?;

        // Print information about which node is being used for this transaction
        println!(
            "Sending transaction for account {} to node at {}",
            signer_account_id,
            client.server_addr()
        );

        let request = RpcSendTransactionRequest { signed_transaction: tx, wait_until };

        client.call(request).await.map_err(|e| e.to_string())
    }
}
