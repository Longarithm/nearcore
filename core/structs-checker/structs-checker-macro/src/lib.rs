use proc_macro::TokenStream;

#[proc_macro_derive(ProtocolStruct)]
pub fn protocol_struct(input: TokenStream) -> TokenStream {
    helper::protocol_struct_impl(input)
}

#[cfg(all(enable_const_type_id, feature = "protocol_schema"))]
mod helper {
    use proc_macro::TokenStream;
    use proc_macro2::TokenStream as TokenStream2;
    use quote::quote;
    use syn::{parse_macro_input, Data, DeriveInput, Fields, FieldsNamed, FieldsUnnamed, Variant};

    fn count_generics(ty: &syn::Type) -> usize {
        fn count_generic_args(args: &syn::PathArguments) -> usize {
            match args {
                syn::PathArguments::AngleBracketed(bracketed) => {
                    bracketed.args.iter().filter(|arg| matches!(arg, syn::GenericArgument::Type(_))).count()
                }
                _ => 0,
            }
        }

        fn count_type(ty: &syn::Type) -> usize {
            match ty {
                syn::Type::Path(type_path) => {
                    type_path.path.segments.iter()
                        .map(|seg| count_generic_args(&seg.arguments))
                        .sum()
                }
                syn::Type::Tuple(tuple) => tuple.elems.iter().map(count_type).sum(),
                syn::Type::Array(array) => count_type(&array.elem),
                syn::Type::Ptr(ptr) => count_type(&ptr.elem),
                syn::Type::Reference(reference) => count_type(&reference.elem),
                syn::Type::Group(group) => count_type(&group.elem),
                syn::Type::Paren(paren) => count_type(&paren.elem),
                syn::Type::Slice(slice) => count_type(&slice.elem),
                _ => 0,
            }
        }

        count_type(ty)
    }
    
    pub fn protocol_struct_impl(input: TokenStream) -> TokenStream {
        let input = parse_macro_input!(input as DeriveInput);
        let name = &input.ident;
        println!("{name}");
        let info_name = quote::format_ident!("{}_INFO", name);

        let type_id = quote! { std::any::TypeId::of::<#name>() };
        let info = match &input.data {
            Data::Struct(data_struct) => {
                let fields = extract_struct_fields(&data_struct.fields);
                quote! {
                    near_structs_checker_lib::ProtocolStructInfo::Struct {
                        name: stringify!(#name),
                        type_id: #type_id,
                        fields: #fields,
                    }
                }
            }
            Data::Enum(data_enum) => {
                let variants = extract_enum_variants(&data_enum.variants);
                quote! {
                    near_structs_checker_lib::ProtocolStructInfo::Enum {
                        name: stringify!(#name),
                        type_id: #type_id,
                        variants: #variants,
                    }
                }
            }
            Data::Union(_) => panic!("Unions are not supported"),
        };

        let expanded = quote! {
            #[allow(non_upper_case_globals)]
            pub static #info_name: near_structs_checker_lib::ProtocolStructInfo = #info;

            near_structs_checker_lib::inventory::submit! {
                #info_name
            }

            impl near_structs_checker_lib::ProtocolStruct for #name {}
        };

        TokenStream::from(expanded)
    }

    fn extract_struct_fields(fields: &Fields) -> TokenStream2 {
        match fields {
            Fields::Named(FieldsNamed { named, .. }) => {
                let fields = extract_from_named_fields(named);
                quote! { &[#(#fields),*] }
            }
            Fields::Unnamed(FieldsUnnamed { unnamed, .. }) => {
                let fields = extract_from_unnamed_fields(unnamed);
                quote! { &[#(#fields),*] }
            }
            Fields::Unit => quote! { &[] },
        }
    }

    fn extract_enum_variants(
        variants: &syn::punctuated::Punctuated<Variant, syn::token::Comma>,
    ) -> TokenStream2 {
        let variants = variants.iter().map(|v| {
            let name = &v.ident;
            let fields = match &v.fields {
                Fields::Named(FieldsNamed { named, .. }) => {
                    let fields = extract_from_named_fields(named);
                    quote! { Some(&[#(#fields),*]) }
                }
                Fields::Unnamed(FieldsUnnamed { unnamed, .. }) => {
                    let fields = extract_from_unnamed_fields(unnamed);
                    quote! { Some(&[#(#fields),*]) }
                }
                Fields::Unit => quote! { None },
            };
            quote! { (stringify!(#name), #fields) }
        });
        quote! { &[#(#variants),*] }
    }

    fn extract_type_info(ty: &syn::Type) -> TokenStream2 {
        let type_path = match ty {
            syn::Type::Path(type_path) => type_path,
            syn::Type::Array(array) => {
                let elem = &array.elem;
                let len = &array.len;
                return quote! { 
                    {
                        const fn create_array() -> [std::any::TypeId; 1] {
                            [std::any::TypeId::of::<#elem>()]
                        }
                        (stringify!([#elem; #len]), &create_array())
                    }
                }
            }
            _ => {
                println!("Unsupported type: {:?}", ty);
                return quote! { (stringify!(#ty), &[std::any::TypeId::of::<#ty>()]) };
            }
        };

        let type_name = quote::format_ident!("{}", type_path.path.segments.last().unwrap().ident);
        let generic_count = count_generics(ty);

        if generic_count == 0 {
            return quote! { (stringify!(#type_name), &[std::any::TypeId::of::<#ty>()]) };
        }

        let generic_params = &type_path.path.segments.last().unwrap().arguments;
        let params = match generic_params {
            syn::PathArguments::AngleBracketed(params) => params,
            _ => return quote! { (stringify!(#type_name), &[std::any::TypeId::of::<#ty>()]) },
        };

        let type_ids: Vec<_> = params.args.iter()
            .filter_map(|arg| {
                if let syn::GenericArgument::Type(ty) = arg {
                    Some(quote! { std::any::TypeId::of::<#ty>() })
                } else {
                    None
                }
            })
            .collect();

        println!("{type_name} | {} {:?}", generic_count, type_ids);
        quote! {
            {
                const GENERIC_COUNT: usize = #generic_count;
                const fn create_array() -> [std::any::TypeId; 1 + GENERIC_COUNT] {
                    [std::any::TypeId::of::<#ty>(), #(#type_ids),*]
                }
                (stringify!(#type_name), &create_array())
            }
        }
    }

    fn extract_from_named_fields(
        named: &syn::punctuated::Punctuated<syn::Field, syn::token::Comma>,
    ) -> impl Iterator<Item = TokenStream2> + '_ {
        named.iter().map(|f| {
            let name = &f.ident;
            let ty = &f.ty;
            let type_info = extract_type_info(ty);
            quote! { (stringify!(#name), #type_info) }
        })
    }

    fn extract_from_unnamed_fields(
        unnamed: &syn::punctuated::Punctuated<syn::Field, syn::token::Comma>,
    ) -> impl Iterator<Item = TokenStream2> + '_ {
        unnamed.iter().enumerate().map(|(i, f)| {
            let index = syn::Index::from(i);
            let ty = &f.ty;
            let type_info = extract_type_info(ty);
            quote! { (stringify!(#index), #type_info) }
        })
    }
}

#[cfg(not(all(enable_const_type_id, feature = "protocol_schema")))]
mod helper {
    use proc_macro::TokenStream;

    pub fn protocol_struct_impl(_input: TokenStream) -> TokenStream {
        TokenStream::new()
    }
}
