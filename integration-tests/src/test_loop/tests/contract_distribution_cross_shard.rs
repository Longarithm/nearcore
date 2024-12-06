use itertools::Itertools;
use near_async::time::Duration;
use near_chain_configs::test_genesis::TestGenesisBuilder;
use near_o11y::testonly::init_test_logger;
use near_primitives::types::AccountId;
use near_vm_runner::ContractCode;

use crate::test_loop::builder::TestLoopBuilder;
use crate::test_loop::env::TestLoopEnv;
use crate::test_loop::utils::contract_distribution::{
    assert_all_chunk_endorsements_received, clear_compiled_contract_caches,
    run_until_caches_contain_contract,
};
use crate::test_loop::utils::transactions::{
    call_contract, check_txs, deploy_contract, make_accounts,
};
use crate::test_loop::utils::{get_head_height, ONE_NEAR};

const EPOCH_LENGTH: u64 = 10;
const GENESIS_HEIGHT: u64 = 1000;

const NUM_BLOCK_AND_CHUNK_PRODUCERS: usize = 4;
const NUM_CHUNK_VALIDATORS_ONLY: usize = 4;
const NUM_RPC: usize = 1;
const NUM_VALIDATORS: usize = NUM_BLOCK_AND_CHUNK_PRODUCERS + NUM_CHUNK_VALIDATORS_ONLY;
const NUM_ACCOUNTS: usize = NUM_VALIDATORS + NUM_RPC;

/// Tests a scenario that different contracts are deployed to a number of accounts and
/// these contracts are called from a set of accounts.
/// Test setup: 2 shards with 9 accounts, for 8 validators and 1 RPC node.
/// Deploys contract to one account from each shard.
/// Make 2 accounts from each shard make calls to these contracts.
#[cfg_attr(not(feature = "test_features"), ignore)]
#[test]
fn test_contract_distribution_cross_shard() {
    init_test_logger();
    let accounts = make_accounts(NUM_ACCOUNTS);

    let (mut env, rpc_id) = setup(&accounts);

    let mut nonce = 1;
    let rpc_index = 8;
    assert_eq!(accounts[rpc_index], rpc_id);

    // Deploy a contract for each shard (account0 from first one, and account4 from second one).
    // Then take two accounts from each shard (one with a contract deployed and one without) and
    // make them call both the contracts, so we cover same-shard and cross-shard contract calls.
    let contract_ids = [&accounts[0], &accounts[4]];
    let sender_ids = [&accounts[0], &accounts[1], &accounts[4], &accounts[5]];

    let start_height = get_head_height(&mut env);

    // First deploy and call the contracts as described above.
    // Next, clear the compiled contract cache and repeat the same contract calls.
    let contracts = deploy_contracts(&mut env, &rpc_id, &contract_ids, &mut nonce);

    for contract in contracts.into_iter() {
        run_until_caches_contain_contract(&mut env, contract.hash());
    }

    call_contracts(&mut env, &rpc_id, &contract_ids, &sender_ids, &mut nonce);

    clear_compiled_contract_caches(&mut env);

    call_contracts(&mut env, &rpc_id, &contract_ids, &sender_ids, &mut nonce);

    let end_height = get_head_height(&mut env);
    assert_all_chunk_endorsements_received(&mut env, start_height, end_height);

    env.shutdown_and_drain_remaining_events(Duration::seconds(20));
}

fn setup(accounts: &Vec<AccountId>) -> (TestLoopEnv, AccountId) {
    let builder = TestLoopBuilder::new();

    let initial_balance = 10000 * ONE_NEAR;
    // All block_and_chunk_producers will be both block and chunk validators.
    let block_and_chunk_producers =
        (0..NUM_BLOCK_AND_CHUNK_PRODUCERS).map(|idx| accounts[idx].as_str()).collect_vec();
    // These are the accounts that are only chunk validators, but not block/chunk producers.
    let chunk_validators_only = (NUM_BLOCK_AND_CHUNK_PRODUCERS..NUM_VALIDATORS)
        .map(|idx| accounts[idx].as_str())
        .collect_vec();

    let clients = accounts.iter().take(NUM_VALIDATORS + NUM_RPC).cloned().collect_vec();
    let rpc_id = accounts[NUM_VALIDATORS].clone();

    let mut genesis_builder = TestGenesisBuilder::new();
    genesis_builder
        .genesis_time_from_clock(&builder.clock())
        .protocol_version_latest()
        .genesis_height(GENESIS_HEIGHT)
        .gas_prices_free()
        .gas_limit_one_petagas()
        .shard_layout_simple_v1(&["account4"])
        .transaction_validity_period(1000)
        .epoch_length(EPOCH_LENGTH)
        .validators_desired_roles(&block_and_chunk_producers, &chunk_validators_only)
        .shuffle_shard_assignment_for_chunk_producers(true)
        .minimum_validators_per_shard(2);
    for account in accounts {
        genesis_builder.add_user_account_simple(account.clone(), initial_balance);
    }
    let (genesis, epoch_config_store) = genesis_builder.build();

    let env =
        builder.genesis(genesis).epoch_config_store(epoch_config_store).clients(clients).build();
    (env, rpc_id)
}

/// Deploys a contract for the given accounts (`contract_ids`) and waits until the transactions are executed.
/// Each account in `contract_ids` gets a fake contract with a different size (thus code-hashes are different)
/// Returns the list of contracts deployed.
fn deploy_contracts(
    env: &mut TestLoopEnv,
    rpc_id: &AccountId,
    contract_ids: &[&AccountId],
    nonce: &mut u64,
) -> Vec<ContractCode> {
    let mut contracts = vec![];
    let mut txs = vec![];
    for (i, contract_id) in contract_ids.into_iter().enumerate() {
        tracing::info!(target: "test", ?rpc_id, ?contract_id, "Deploying contract.");
        let contract =
            ContractCode::new(near_test_contracts::sized_contract((i + 1) * 100).to_vec(), None);
        let tx = deploy_contract(
            &mut env.test_loop,
            &env.datas,
            rpc_id,
            contract_id,
            contract.code().to_vec(),
            *nonce,
        );
        txs.push(tx);
        *nonce += 1;
        contracts.push(contract);
    }
    env.test_loop.run_for(Duration::seconds(2));
    check_txs(&env.test_loop, &env.datas, rpc_id, &txs);
    contracts
}

/// Makes calls to the contracts from sender_ids to the contract_ids (at which contracts are deployed).
fn call_contracts(
    env: &mut TestLoopEnv,
    rpc_id: &AccountId,
    contract_ids: &[&AccountId],
    sender_ids: &[&AccountId],
    nonce: &mut u64,
) {
    let method_name = "main".to_owned();
    let mut txs = vec![];
    for sender_id in sender_ids.into_iter() {
        for contract_id in contract_ids.into_iter() {
            tracing::info!(target: "test", ?rpc_id, ?sender_id, ?contract_id, "Calling contract.");
            let tx = call_contract(
                &mut env.test_loop,
                &env.datas,
                rpc_id,
                sender_id,
                contract_id,
                method_name.clone(),
                vec![],
                *nonce,
            );
            txs.push(tx);
            *nonce += 1;
        }
    }
    env.test_loop.run_for(Duration::seconds(2));
    check_txs(&env.test_loop, &env.datas, &rpc_id, &txs);
}