// boxel: modified by tools/refork (crate renames, platform shim, test-only patches or backports).
use boxel_macro_utils::fq_std::{FQSend, FQSync};
use proc_macro::TokenStream;
use quote::quote;
use syn::{parse_macro_input, parse_quote, DeriveInput, Path};

pub fn derive_message(input: TokenStream) -> TokenStream {
    let mut ast = parse_macro_input!(input as DeriveInput);
    let boxel_ecs_path: Path = crate::boxel_ecs_path();

    ast.generics
        .make_where_clause()
        .predicates
        .push(parse_quote! { Self: #FQSend + #FQSync + 'static });

    let struct_name = &ast.ident;
    let (impl_generics, type_generics, where_clause) = &ast.generics.split_for_impl();

    TokenStream::from(quote! {
        impl #impl_generics #boxel_ecs_path::message::Message for #struct_name #type_generics #where_clause {}
    })
}
