use crate::test_loop::builder::TestLoopBuilder;
use crate::test_loop::env::{TestData, TestLoopEnv};
use crate::test_loop::utils::transactions::{call_contract, deploy_contract, get_node_data};
use crate::test_loop::utils::ONE_NEAR;
use assert_matches::assert_matches;
use near_async::test_loop::data::TestLoopData;
use near_async::test_loop::TestLoopV2;
use near_async::time::{Clock, Duration};
use near_chain_configs::test_genesis::TestGenesisBuilder;
use near_client::Client;
use near_o11y::testonly::init_test_logger;
use near_primitives::types::AccountId;
use near_primitives::views::FinalExecutionStatus;
use near_primitives_core::hash::CryptoHash;
use near_primitives_core::types::ProtocolVersion;
use near_primitives_core::version::{ProtocolFeature, PROTOCOL_VERSION};
use near_store::get_account;
use std::io::Read;
use std::string::ToString;

#[test]
fn run() {
    init_test_logger();
    let builder = TestLoopBuilder::new();

    let initial_balance = 10000 * ONE_NEAR;
    let epoch_length = 5;
    let contract_id: AccountId = "contractregistry.testnet".parse().unwrap();
    let accounts = (0..3)
        .map(|i| format!("account{}", i).parse().unwrap())
        .chain(vec![contract_id.clone()])
        .collect::<Vec<AccountId>>();
    let clients = accounts.clone();
    let accounts_str: Vec<_> = accounts.iter().map(|a| a.as_str()).collect();
    let (validators_str, rpc_id) = accounts_str.split_at(2);
    let rpc_id = rpc_id[0].parse().unwrap();

    let mut genesis_builder = TestGenesisBuilder::new();
    let target_protocol_version =
        ProtocolFeature::RemoveAccountWithLongStorageKey.protocol_version();
    genesis_builder
        .genesis_time_from_clock(&builder.clock())
        .protocol_version(target_protocol_version - 1)
        .shard_layout_simple_v1(&["account2"])
        .epoch_length(epoch_length)
        .validators_desired_roles(validators_str, &[]);
    for account in &accounts {
        genesis_builder.add_user_account_simple(account.clone(), initial_balance);
    }
    let genesis = genesis_builder.build();

    set_upgrade_schedule(
        builder.clock(),
        vec![
            (Duration::seconds(60), target_protocol_version),
            (Duration::seconds(80), PROTOCOL_VERSION),
        ],
    );

    let TestLoopEnv { mut test_loop, datas: node_datas, tempdir } =
        builder.genesis(genesis).clients(clients).build();

    do_deploy_contract(&mut test_loop, &node_datas, &rpc_id, &contract_id);
    do_call_contract(
        &mut test_loop,
        &node_datas,
        &rpc_id,
        &contract_id,
        &vec![contract_id.clone()],
    );
    let rpc = rpc_client(&test_loop, &node_datas, &rpc_id);
    let tip = rpc.chain.head().unwrap();
    let block = rpc.chain.get_block(&tip.last_block_hash).unwrap();
    let prev_state_root = block.chunks().get(1).unwrap().prev_state_root();
    let trie = rpc
        .runtime_adapter
        .get_view_trie_for_shard(1, &tip.prev_block_hash, prev_state_root)
        .unwrap();
    println!("{:?}", get_account(&trie, &contract_id));

    let client_handle = node_datas[0].client_sender.actor_handle();

    let success_condition = |test_loop_data: &mut TestLoopData| -> bool {
        let client = &test_loop_data.get(&client_handle).client;
        let tip = client.chain.head().unwrap();
        let pv = client.epoch_manager.get_epoch_protocol_version(&tip.epoch_id).unwrap();
        // println!("{} {}", tip.height, pv);
        return false;
    };

    test_loop.run_until(success_condition, Duration::seconds(60));

    TestLoopEnv { test_loop, datas: node_datas, tempdir }
        .shutdown_and_drain_remaining_events(Duration::seconds(20));
}

/// Deploy the contract and wait until the transaction is executed.
fn do_deploy_contract(
    test_loop: &mut TestLoopV2,
    node_datas: &Vec<TestData>,
    rpc_id: &AccountId,
    contract_id: &AccountId,
) {
    tracing::info!(target: "test", ?rpc_id, ?contract_id, "Deploying contract.");
    let tx = deploy_contract(test_loop, node_datas, rpc_id, contract_id);
    test_loop.run_for(Duration::seconds(20));
    // check_txs(&*test_loop, node_datas, rpc_id, &[tx]);
}

/// Call the contract from all accounts and wait until the transactions are executed.
fn do_call_contract(
    test_loop: &mut TestLoopV2,
    node_datas: &Vec<TestData>,
    rpc_id: &AccountId,
    contract_id: &AccountId,
    accounts: &Vec<AccountId>,
) {
    tracing::info!(target: "test", ?rpc_id, ?contract_id, "Calling contract.");
    let mut txs = vec![];
    for sender_id in accounts {
        let tx = call_contract(
            test_loop,
            node_datas,
            &sender_id,
            &contract_id,
            "write_one_megabyte".to_string(),
            1u8.to_le_bytes().to_vec(),
        );
        txs.push(tx);
    }
    test_loop.run_for(Duration::seconds(20));
    // check_txs(&*test_loop, node_datas, &rpc_id, &txs);
}

/// Get the client for the provided rpd node account id.
fn rpc_client<'a>(
    test_loop: &'a TestLoopV2,
    node_datas: &'a Vec<TestData>,
    rpc_id: &AccountId,
) -> &'a Client {
    let node_data = get_node_data(node_datas, rpc_id);
    let client_actor_handle = node_data.client_sender.actor_handle();
    let client_actor = test_loop.data.get(&client_actor_handle);
    &client_actor.client
}

/// Check the status of the transactions and assert that they are successful.
///
/// Please note that it's important to use an rpc node that tracks all shards.
/// Otherwise, the transactions may not be found.
fn check_txs(
    test_loop: &TestLoopV2,
    node_datas: &Vec<TestData>,
    rpc: &AccountId,
    txs: &[CryptoHash],
) {
    let rpc = rpc_client(test_loop, node_datas, rpc);

    for &tx in txs {
        let tx_outcome = rpc.chain.get_partial_transaction_result(&tx);
        let status = tx_outcome.as_ref().map(|o| o.status.clone());
        println!("{:?}", status);
        let status = status.unwrap();
        tracing::info!(target: "test", ?tx, ?status, "transaction status");
        assert_matches!(status, FinalExecutionStatus::SuccessValue(_));
    }
}

fn set_upgrade_schedule(clock: Clock, schedule: Vec<(Duration, ProtocolVersion)>) {
    let mut result = vec![];
    let now = clock.now_utc();
    for (timedelta, version) in schedule {
        let upgrade_time = now + timedelta;
        let upgrade_time_chrono = chrono::DateTime::from_timestamp(
            upgrade_time.unix_timestamp(),
            upgrade_time.nanosecond(),
        )
        .unwrap();
        let upgrade_time_str = upgrade_time_chrono.format("%Y-%m-%d %H:%M:%S").to_string();
        result.push(format!("{}={}", upgrade_time_str, version));
    }
    let protocol_version_override = result.join(",");
    tracing::info!(target: "test", ?protocol_version_override, "setting the protocol_version_override");
    std::env::set_var("NEAR_TESTS_PROTOCOL_UPGRADE_OVERRIDE", protocol_version_override);
}
