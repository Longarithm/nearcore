// use near_primitives::epoch_manager::EpochConfig;
// use std::collections::BTreeMap;
// use std::sync::Arc;

// #[derive(Clone, Debug)]
// pub struct EpochConfigStore {
//     /// Maps protocol version to the config.
//     store: BTreeMap<ProtocolVersion, Arc<EpochConfig>>,
// }

#[cfg(test)]
mod tests {
    use near_chain_configs::{Genesis, GenesisValidationMode};
    use near_primitives::epoch_manager::{AllEpochConfig, EpochConfig};
    use near_primitives::version::PROTOCOL_VERSION;

    #[test]
    fn run() {
        let genesis = Genesis::from_file(
            "../../utils/mainnet-res/res/mainnet_genesis.json",
            GenesisValidationMode::Full,
        )
        .unwrap();
        let genesis_config = genesis.config;
        let chain_id = genesis_config.chain_id.clone();
        let initial_epoch_config = EpochConfig::from(genesis_config);
        let all_config =
            AllEpochConfig::new_with_test_overrides(true, initial_epoch_config, &chain_id, None);

        println!("{:?}", all_config.for_protocol_version(PROTOCOL_VERSION));
    }
}
