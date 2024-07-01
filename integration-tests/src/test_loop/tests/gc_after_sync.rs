use crate::test_loop::builder::TestLoopBuilder;
use crate::test_loop::env::TestLoopEnv;
use crate::test_loop::utils::ONE_NEAR;
use itertools::Itertools;
use near_async::test_loop::data::TestLoopData;
use near_async::time::Duration;
use near_chain_configs::test_genesis::TestGenesisBuilder;
use near_o11y::testonly::init_test_logger;
use near_primitives::types::AccountId;

#[test]
fn test_gc_after_sync() {
    init_test_logger();
    let builder = TestLoopBuilder::new();

    let initial_balance = 10000 * ONE_NEAR;
    let epoch_length = 10;
    let accounts =
        (0..10).map(|i| format!("account{}", i).parse().unwrap()).collect::<Vec<AccountId>>();
    let clients = accounts.iter().take(2).cloned().collect_vec();
    let validators = clients.iter().take(1).map(|a| a.as_str()).collect_vec();

    let mut genesis_builder = TestGenesisBuilder::new();
    let genesis_height = 10000;
    genesis_builder
        .genesis_time_from_clock(&builder.clock())
        .genesis_height(genesis_height)
        .shard_layout_simple_v1(&[])
        .epoch_length(epoch_length)
        .validators_desired_roles(&validators, &[]);
    for account in &accounts {
        genesis_builder.add_user_account_simple(account.clone(), initial_balance);
    }
    let genesis = genesis_builder.build();

    let TestLoopEnv { mut test_loop, datas: node_datas } =
        builder.genesis(genesis).clients(clients).track_all_shards().build();

    let client_handle = node_datas[0].client_sender.actor_handle();
    let client2_handle = node_datas[1].client_sender.actor_handle();
    test_loop.run_until(
        |test_loop_data: &mut TestLoopData| -> bool {
            let client2 = &test_loop_data.get(&client2_handle).client;
            let tip = client2.chain.head().unwrap();
            println!(
                "{} {}",
                tip.height,
                client2.shard_tracker.care_about_shard(None, &tip.prev_block_hash, 0, false)
            );

            let client = &test_loop_data.get(&client_handle).client;
            let tip = client.chain.head().unwrap();
            tip.height > genesis_height + epoch_length * 3 / 2
        },
        Duration::seconds((3 * epoch_length) as i64),
    );

    TestLoopEnv { test_loop, datas: node_datas }
        .shutdown_and_drain_remaining_events(Duration::seconds(20));
}
