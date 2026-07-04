//! Proc macros for cntryl-stress.

use proc_macro::TokenStream;
use quote::quote;
use syn::parse::Parser;
use syn::{parse_macro_input, Expr, ExprLit, ItemFn, Lit, Meta, MetaNameValue, Token};

/// Mark a function as a stress benchmark.
///
/// Common usage stays intentionally small:
///
/// ```rust,ignore
/// use cntryl_stress::{stress_test, StressContext};
///
/// #[stress_test(tier = 2)]
/// fn write_batch(ctx: &mut StressContext) {
///     ctx.parameter("payload_size", 4096);
///     ctx.measure(|| write_the_batch());
/// }
/// ```
///
/// Supported attributes:
///
/// - `tier = 2` through `N` (defaults to `2`)
/// - `mode = "fixed_operations"` or `mode = "fixed_duration"`
/// - `name = "custom_name"`
/// - `ignore`
/// - `metadata(owner = "storage", scenario = "fanout")`
#[proc_macro_attribute]
pub fn stress_test(attr: TokenStream, item: TokenStream) -> TokenStream {
    let input = parse_macro_input!(item as ItemFn);
    let fn_name = &input.sig.ident;
    let fn_name_str = fn_name.to_string();

    let attrs = match StressAttrs::parse(attr) {
        Ok(attrs) => attrs,
        Err(error) => return error.to_compile_error().into(),
    };
    if attrs.tier < 2 {
        return syn::Error::new_spanned(
            fn_name,
            "cntryl-stress tiers start at 2; use Criterion for Tier 1 microbenchmarks",
        )
        .to_compile_error()
        .into();
    }

    let benchmark_name = attrs.name.unwrap_or_else(|| fn_name_str.clone());
    let is_ignored = attrs.ignore;
    let tier = attrs.tier;
    let mode = match attrs.mode.as_str() {
        "fixed_duration" => quote! { ::cntryl_stress::BenchmarkModeKind::FixedDuration },
        "fixed_operations" => quote! { ::cntryl_stress::BenchmarkModeKind::FixedOperations },
        other => {
            return syn::Error::new_spanned(
                fn_name,
                format!(
                    "unsupported stress_test mode '{other}'; expected fixed_operations or fixed_duration"
                ),
            )
            .to_compile_error()
            .into();
        }
    };
    let metadata_keys = attrs.metadata.iter().map(|(key, _)| key);
    let metadata_values = attrs.metadata.iter().map(|(_, value)| value);
    let submit_ident = syn::Ident::new(
        &format!("__STRESS_BENCH_{}", fn_name_str.to_uppercase()),
        fn_name.span(),
    );

    quote! {
        #input

        #[allow(non_upper_case_globals)]
        #[::cntryl_stress::__private::linkme::distributed_slice(::cntryl_stress::__private::STRESS_BENCHMARKS)]
        #[linkme(crate = ::cntryl_stress::__private::linkme)]
        static #submit_ident: ::cntryl_stress::__private::BenchmarkEntry = ::cntryl_stress::__private::BenchmarkEntry {
            name: #benchmark_name,
            func: #fn_name,
            ignored: #is_ignored,
            module_path: module_path!(),
            tier: #tier,
            mode: #mode,
            metadata: &[#((#metadata_keys, #metadata_values)),*],
        };
    }
    .into()
}

#[derive(Debug)]
struct StressAttrs {
    name: Option<String>,
    tier: u32,
    mode: String,
    ignore: bool,
    metadata: Vec<(String, String)>,
}

impl Default for StressAttrs {
    fn default() -> Self {
        Self {
            name: None,
            tier: 2,
            mode: "fixed_operations".to_string(),
            ignore: false,
            metadata: Vec::new(),
        }
    }
}

impl StressAttrs {
    fn parse(attr: TokenStream) -> syn::Result<Self> {
        let parser = syn::punctuated::Punctuated::<Meta, Token![,]>::parse_terminated;
        let metas = parser.parse(attr)?;
        let mut attrs = Self::default();

        for meta in metas {
            match meta {
                Meta::Path(path) if path.is_ident("ignore") => attrs.ignore = true,
                Meta::NameValue(name_value) if name_value.path.is_ident("name") => {
                    attrs.name = Some(string_value(&name_value)?);
                }
                Meta::NameValue(name_value) if name_value.path.is_ident("tier") => {
                    attrs.tier = int_value(&name_value)?;
                }
                Meta::NameValue(name_value) if name_value.path.is_ident("mode") => {
                    attrs.mode = string_value(&name_value)?;
                }
                Meta::List(list) if list.path.is_ident("metadata") => {
                    attrs.metadata.extend(parse_metadata(list.tokens)?);
                }
                other => {
                    return Err(syn::Error::new_spanned(
                        other,
                        "unsupported stress_test attribute",
                    ));
                }
            }
        }

        Ok(attrs)
    }
}

fn parse_metadata(tokens: proc_macro2::TokenStream) -> syn::Result<Vec<(String, String)>> {
    let parser = syn::punctuated::Punctuated::<MetaNameValue, Token![,]>::parse_terminated;
    parser
        .parse2(tokens)?
        .into_iter()
        .map(|name_value| {
            let key = name_value
                .path
                .get_ident()
                .map(ToString::to_string)
                .ok_or_else(|| {
                    syn::Error::new_spanned(&name_value.path, "metadata keys must be identifiers")
                })?;
            let value = string_value(&name_value)?;
            Ok((key, value))
        })
        .collect()
}

fn string_value(name_value: &MetaNameValue) -> syn::Result<String> {
    match &name_value.value {
        Expr::Lit(ExprLit {
            lit: Lit::Str(value),
            ..
        }) => Ok(value.value()),
        value => Err(syn::Error::new_spanned(value, "expected string literal")),
    }
}

fn int_value(name_value: &MetaNameValue) -> syn::Result<u32> {
    match &name_value.value {
        Expr::Lit(ExprLit {
            lit: Lit::Int(value),
            ..
        }) => value.base10_parse(),
        value => Err(syn::Error::new_spanned(value, "expected integer literal")),
    }
}

/// Generate a `main` function for stress benchmark binaries.
#[proc_macro]
pub fn stress_main(_input: TokenStream) -> TokenStream {
    quote! {
        fn main() {
            ::cntryl_stress::stress_binary_main();
        }
    }
    .into()
}
