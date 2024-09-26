use itertools::Itertools;
use near_chain_configs::test_genesis::TestGenesisBuilder;
use near_o11y::testonly::init_test_logger;
use near_primitives::shard_layout::ShardLayout;
use near_primitives::types::AccountId;
use near_primitives::version::ProtocolFeature;

use crate::test_loop::builder::TestLoopBuilder;
use crate::test_loop::env::TestLoopEnv;
use crate::test_loop::utils::ONE_NEAR;

#[test]
fn test_resharding_v3() {
    init_test_logger();
    let builder = TestLoopBuilder::new();

    let initial_balance = 1_000_000 * ONE_NEAR;
    let epoch_length = 10;
    let accounts =
        (0..8).map(|i| format!("account{}", i).parse().unwrap()).collect::<Vec<AccountId>>();
    let clients = accounts.iter().cloned().collect_vec();

    let mut genesis_builder = TestGenesisBuilder::new();
    genesis_builder
        .genesis_time_from_clock(&builder.clock())
        .shard_layout(ShardLayout::get_simple_nightshade_layout_v3())
        .protocol_version(ProtocolFeature::SimpleNightshadeV4.protocol_version() - 1)
        .epoch_length(epoch_length);
    for account in &accounts {
        genesis_builder.add_user_account_simple(account.clone(), initial_balance);
    }
    let genesis = genesis_builder.build();

    let TestLoopEnv { mut test_loop, datas: node_datas, tempdir } =
        builder.genesis(genesis).clients(clients).build();
}
