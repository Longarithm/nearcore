use near_chain_configs::GenesisValidationMode;
use near_o11y::tracing::{info, warn};
use near_store::{create_store_with_config, decode_value_with_rc, DBCol, RawTrieNodeWithSize};
use nearcore::{get_default_home, get_store_path, load_config};

fn main() -> std::io::Result<()> {
    let env_filter = near_o11y::EnvFilterBuilder::from_env().verbose(Some("")).finish().unwrap();
    let _subscriber = near_o11y::default_subscriber(env_filter).global();
    info!(target: "rocksdb-state-player", "Start");

    let home_dir = get_default_home();
    let near_config = load_config(&home_dir, GenesisValidationMode::UnsafeFast)
        .unwrap_or_else(|e| panic!("Error loading config: {:#}", e));
    let store_path = get_store_path(&home_dir);
    let store_config = &near_config.config.store.clone().with_read_only(true);
    let store = create_store_with_config(&store_path, store_config);

    let mut store_update = store.store_update();
    let mut i: u64 = 0;
    let mut nodes: u64 = 0;
    let mut values: u64 = 0;
    let batch_size = 1000;
    for (key, bytes) in store.iter_raw_bytes(DBCol::State) {
        i += 1;
        if i % batch_size == 0 {
            info!(target: "rocksdb-state-player", "processed: {} nodes: {} values: {}", i, nodes, values);
            store_update.commit().unwrap();
            store_update = store.store_update();
        }
        let (value, _) = decode_value_with_rc(&bytes);
        let result = match value {
            Some(value) => RawTrieNodeWithSize::decode(&value),
            None => {
                warn!(target: "rocksdb-state-player", "couldn't decode rc from: {:?} {:?}", key, bytes);
            }
        };
        match result {
            Ok(_) => {
                nodes += 1;
                store_update.set(DBCol::StateNode, &key, &bytes);
            }
            Err(_) => {
                values += 1;
                store_update.set(DBCol::StateValue, &key, &bytes);
            }
        }
    }
    store_update.commit().unwrap();

    Ok(())
}
