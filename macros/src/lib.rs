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
/// use cntryl_stress::{black_box, stress_test, StressContext};
///
/// #[stress_test(tier = 1)]
/// fn parse_hot_path(ctx: &mut StressContext) {
///     let header = b"content-type:application/json";
///     ctx.measure_micro(|| black_box(header.iter().position(|byte| *byte == b':')));
/// }
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
/// - `tier = 1` through `N` (defaults to `2`)
/// - `mode = "micro"`, `mode = "fixed_operations"`, or `mode = "fixed_duration"`
/// - `name = "custom_name"`
/// - `ignore`
/// - `max_ns_per_op = 1000`
/// - `max_allocs_per_op = 0`
/// - `max_bytes_per_op = 0`
/// - `max_regression_pct = 5`
/// - `max_rsd_pct = 10`
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
    if attrs.tier == 0 {
        return syn::Error::new_spanned(fn_name, "cntryl-stress tiers start at 1")
            .to_compile_error()
            .into();
    }

    let benchmark_name = attrs.name.unwrap_or_else(|| fn_name_str.clone());
    let is_ignored = attrs.ignore;
    let tier = attrs.tier;
    let mode_name = attrs.mode.unwrap_or_else(|| {
        if attrs.tier == 1 {
            "micro".to_string()
        } else {
            "fixed_operations".to_string()
        }
    });
    let mode = match mode_name.as_str() {
        "micro" => quote! { ::cntryl_stress::BenchmarkModeKind::Micro },
        "fixed_duration" => quote! { ::cntryl_stress::BenchmarkModeKind::FixedDuration },
        "fixed_operations" => quote! { ::cntryl_stress::BenchmarkModeKind::FixedOperations },
        other => {
            return syn::Error::new_spanned(
                fn_name,
                format!(
                    "unsupported stress_test mode '{other}'; expected micro, fixed_operations, or fixed_duration"
                ),
            )
            .to_compile_error()
            .into();
        }
    };
    let max_ns_per_op = option_f64_tokens(attrs.budgets.ns_per_op);
    let max_allocs_per_op = option_f64_tokens(attrs.budgets.allocs_per_op);
    let max_bytes_per_op = option_f64_tokens(attrs.budgets.bytes_per_op);
    let max_regression_pct = option_f64_tokens(attrs.budgets.regression_pct);
    let max_rsd_pct = option_f64_tokens(attrs.budgets.rsd_pct);
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
            budgets: ::cntryl_stress::BenchmarkBudgets {
                max_ns_per_op: #max_ns_per_op,
                max_allocs_per_op: #max_allocs_per_op,
                max_bytes_per_op: #max_bytes_per_op,
                max_regression_pct: #max_regression_pct,
                max_rsd_pct: #max_rsd_pct,
            },
            metadata: &[#((#metadata_keys, #metadata_values)),*],
        };
    }
    .into()
}

#[derive(Debug)]
struct StressAttrs {
    name: Option<String>,
    tier: u32,
    mode: Option<String>,
    ignore: bool,
    budgets: StressBudgets,
    metadata: Vec<(String, String)>,
}

#[derive(Debug, Default)]
struct StressBudgets {
    ns_per_op: Option<f64>,
    allocs_per_op: Option<f64>,
    bytes_per_op: Option<f64>,
    regression_pct: Option<f64>,
    rsd_pct: Option<f64>,
}

impl Default for StressAttrs {
    fn default() -> Self {
        Self {
            name: None,
            tier: 2,
            mode: None,
            ignore: false,
            budgets: StressBudgets::default(),
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
                    attrs.mode = Some(string_value(&name_value)?);
                }
                Meta::NameValue(name_value) if name_value.path.is_ident("max_ns_per_op") => {
                    attrs.budgets.ns_per_op = Some(float_value(&name_value)?);
                }
                Meta::NameValue(name_value) if name_value.path.is_ident("max_allocs_per_op") => {
                    attrs.budgets.allocs_per_op = Some(float_value(&name_value)?);
                }
                Meta::NameValue(name_value) if name_value.path.is_ident("max_bytes_per_op") => {
                    attrs.budgets.bytes_per_op = Some(float_value(&name_value)?);
                }
                Meta::NameValue(name_value) if name_value.path.is_ident("max_regression_pct") => {
                    attrs.budgets.regression_pct = Some(float_value(&name_value)?);
                }
                Meta::NameValue(name_value) if name_value.path.is_ident("max_rsd_pct") => {
                    attrs.budgets.rsd_pct = Some(float_value(&name_value)?);
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

fn float_value(name_value: &MetaNameValue) -> syn::Result<f64> {
    match &name_value.value {
        Expr::Lit(ExprLit {
            lit: Lit::Float(value),
            ..
        }) => value.base10_parse(),
        Expr::Lit(ExprLit {
            lit: Lit::Int(value),
            ..
        }) => value.base10_parse(),
        value => Err(syn::Error::new_spanned(value, "expected numeric literal")),
    }
}

fn option_f64_tokens(value: Option<f64>) -> proc_macro2::TokenStream {
    if let Some(value) = value {
        quote! { Some(#value) }
    } else {
        quote! { None }
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
