use near_structs_checker_core::{register_protocol_struct, ProtocolStructInfo};
use proc_macro::TokenStream;
use quote::{quote, ToTokens};
use std::collections::BTreeMap;
use syn::{parse_macro_input, Data, DeriveInput, Fields};

fn parse_protocol_struct(input: &DeriveInput) -> ProtocolStructInfo {
    let name = input.ident.to_string();
    let mut fields = BTreeMap::new();

    match &input.data {
        Data::Struct(data) => match &data.fields {
            Fields::Named(named_fields) => {
                for field in &named_fields.named {
                    let field_name = field.ident.as_ref().unwrap().to_string();
                    let field_type = field.ty.to_token_stream().to_string();
                    fields.insert(field_name, field_type);
                }
            }
            Fields::Unnamed(unnamed_fields) => {
                for (index, field) in unnamed_fields.unnamed.iter().enumerate() {
                    let field_name = index.to_string();
                    let field_type = field.ty.to_token_stream().to_string();
                    fields.insert(field_name, field_type);
                }
            }
            Fields::Unit => {}
        },
        _ => panic!("ProtocolStruct can only be derived for structs"),
    }

    // Calculate hash (you may want to implement a more sophisticated hash function)
    let hash = calculate_hash(&name, &fields);

    ProtocolStructInfo { name, fields, hash }
}

fn calculate_hash(name: &str, fields: &BTreeMap<String, String>) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut hasher = DefaultHasher::new();
    name.hash(&mut hasher);
    for (key, value) in fields {
        key.hash(&mut hasher);
        value.hash(&mut hasher);
    }
    hasher.finish()
}

#[proc_macro_derive(ProtocolStruct)]
pub fn protocol_struct(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    let info = parse_protocol_struct(&input);

    println!("Inserting struct info for: {}", info.name);
    register_protocol_struct(info.name.clone(), info.clone());

    // Generate the implementation
    let name = &input.ident;
    let hash = info.hash;

    let expanded = quote! {
        impl near_structs_checker_core::ProtocolStruct for #name {
            fn calculate_hash() -> u64 {
                #hash
            }
        }
    };

    TokenStream::from(expanded)
}
