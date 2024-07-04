use serde::{Deserialize, Serialize};
use std::any::TypeId;
use std::collections::hash_map::DefaultHasher;
use std::collections::BTreeMap;
use std::hash::{Hash, Hasher};

use once_cell::sync::Lazy;
use std::sync::Mutex;

#[derive(Clone, Serialize, Deserialize)]
pub struct FieldInfo {
    pub name: String,
    pub type_name: String,
    pub hash: u64,
}

// Define the structure to hold protocol struct information
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ProtocolStructInfo {
    pub name: String,
    pub fields: BTreeMap<String, String>,
    pub hash: u64,
}

pub trait ProtocolStruct {
    fn type_name() -> String {
        std::any::type_name::<Self>().to_string()
    }

    fn calculate_hash() -> u64 {
        let mut hasher = DefaultHasher::new();
        Self::type_name().hash(&mut hasher);
        hasher.finish()
    }
}

// Implement ProtocolStruct for primitive types
macro_rules! impl_protocol_struct_for_primitive {
    ($($t:ty),*) => {
        $(
            impl ProtocolStruct for $t {}
        )*
    }
}

impl_protocol_struct_for_primitive!(u8, u16, u32, u64, u128, i8, i16, i32, i64, i128, bool, String);

// Implement for arrays
impl<T: ProtocolStruct, const N: usize> ProtocolStruct for [T; N] {}

pub fn calculate_struct_hash<T: 'static>() -> u64 {
    let mut hasher = DefaultHasher::new();
    TypeId::of::<T>().hash(&mut hasher);
    hasher.finish()
}

pub type ProtocolStructs = BTreeMap<String, ProtocolStructInfo>;

struct ProtocolStructsWrapper(ProtocolStructs);

static PROTOCOL_STRUCTS: Lazy<Mutex<ProtocolStructsWrapper>> =
    Lazy::new(|| Mutex::new(ProtocolStructsWrapper(BTreeMap::new())));

pub fn register_protocol_struct(name: String, info: ProtocolStructInfo) {
    println!("Registering: {}", name);
    PROTOCOL_STRUCTS.lock().unwrap().0.insert(name, info);
}

impl Drop for ProtocolStructsWrapper {
    fn drop(&mut self) {
        println!("Size!: {}", self.0.len());
        let json = serde_json::to_string_pretty(&self.0).unwrap();
        std::fs::write("protocol_structs.json", json).unwrap();
    }
}
