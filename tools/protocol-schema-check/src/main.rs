use near_structs_checker::ProtocolStruct;
use std::hash::Hash;

#[derive(Debug, ProtocolStruct)]
pub struct CryptoHash(pub [u8; 32]);

fn main() {
    println!("{:?}", "Hello world");
    // let json = serde_json::to_string_pretty(&protocol_structs).unwrap();
    // std::fs::write("protocol_structs.json", json).unwrap();
    // println!("{:?}", CryptoHash);
}
