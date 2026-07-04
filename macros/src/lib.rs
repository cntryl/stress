//! Proc macros for cntryl-stress.

use proc_macro::TokenStream;
use quote::quote;
use syn::parse::Parser;
use syn::{parse_macro_input, Expr, ExprLit, ItemFn, Lit, Meta, MetaNameValue, Token};

const MAX_TIER: u32 = 6;

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
/// - `tier = 1` through `6` (defaults to `2`)
/// - `mode = "micro"`, `mode = "fixed_operations"`, or `mode = "fixed_duration"` when it matches the tier-derived mode
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
    if let Some(error) = tier_error(attrs.tier) {
        return syn::Error::new_spanned(fn_name, error)
            .to_compile_error()
            .into();
    }

    let benchmark_name = attrs.name.unwrap_or_else(|| fn_name_str.clone());
    let is_ignored = attrs.ignore;
    let tier = attrs.tier;
    let Some(derived_mode) = mode_for_tier(tier) else {
        return syn::Error::new_spanned(fn_name, "cntryl-stress tiers are 1 through 6")
            .to_compile_error()
            .into();
    };
    let mode_kind = match attrs.mode.as_deref() {
        Some(mode_name) => {
            let Some(mode_kind) = parse_mode_kind(mode_name) else {
                return syn::Error::new_spanned(
                    fn_name,
                    format!(
                        "unsupported stress_test mode '{mode_name}'; expected micro, fixed_operations, or fixed_duration"
                    ),
                )
                .to_compile_error()
                .into();
            };
            if mode_kind != derived_mode {
                return syn::Error::new_spanned(
                    fn_name,
                    tier_mode_mismatch_message(tier, derived_mode, mode_kind),
                )
                .to_compile_error()
                .into();
            }
            mode_kind
        }
        None => derived_mode,
    };
    let mode = mode_kind.tokens();
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ModeKind {
    Micro,
    FixedOperations,
    FixedDuration,
}

impl ModeKind {
    fn tokens(self) -> proc_macro2::TokenStream {
        match self {
            Self::Micro => quote! { ::cntryl_stress::BenchmarkModeKind::Micro },
            Self::FixedOperations => {
                quote! { ::cntryl_stress::BenchmarkModeKind::FixedOperations }
            }
            Self::FixedDuration => quote! { ::cntryl_stress::BenchmarkModeKind::FixedDuration },
        }
    }

    const fn as_str(self) -> &'static str {
        match self {
            Self::Micro => "micro",
            Self::FixedOperations => "fixed_operations",
            Self::FixedDuration => "fixed_duration",
        }
    }
}

fn parse_mode_kind(value: &str) -> Option<ModeKind> {
    match value {
        "micro" => Some(ModeKind::Micro),
        "fixed_operations" => Some(ModeKind::FixedOperations),
        "fixed_duration" => Some(ModeKind::FixedDuration),
        _ => None,
    }
}

const fn mode_for_tier(tier: u32) -> Option<ModeKind> {
    match tier {
        1 => Some(ModeKind::Micro),
        2 => Some(ModeKind::FixedOperations),
        3..=MAX_TIER => Some(ModeKind::FixedDuration),
        _ => None,
    }
}

fn tier_mode_mismatch_message(tier: u32, expected: ModeKind, actual: ModeKind) -> String {
    format!(
        "Tier {tier} uses {}; remove mode or use {} for {}.",
        expected.as_str(),
        tier_hint_for_mode(actual),
        actual.as_str()
    )
}

const fn tier_hint_for_mode(mode: ModeKind) -> &'static str {
    match mode {
        ModeKind::Micro => "tier = 1",
        ModeKind::FixedOperations => "tier = 2",
        ModeKind::FixedDuration => "tier = 3",
    }
}

fn tier_error(tier: u32) -> Option<String> {
    if tier == 0 {
        Some("cntryl-stress tiers start at 1".to_string())
    } else if tier > MAX_TIER {
        Some(format!("cntryl-stress tiers are 1 through {MAX_TIER}"))
    } else {
        None
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tier_validation_rejects_undefined_tiers() {
        assert_eq!(
            tier_error(0).as_deref(),
            Some("cntryl-stress tiers start at 1")
        );
        assert_eq!(
            tier_error(MAX_TIER + 1).as_deref(),
            Some("cntryl-stress tiers are 1 through 6")
        );
        assert!(tier_error(1).is_none());
        assert!(tier_error(MAX_TIER).is_none());
    }

    #[test]
    fn mode_defaults_are_derived_from_tier() {
        assert_eq!(mode_for_tier(1), Some(ModeKind::Micro));
        assert_eq!(mode_for_tier(2), Some(ModeKind::FixedOperations));
        for tier in 3..=MAX_TIER {
            assert_eq!(mode_for_tier(tier), Some(ModeKind::FixedDuration));
        }
    }

    #[test]
    fn mode_mismatch_guidance_points_to_matching_tier() {
        assert_eq!(
            tier_mode_mismatch_message(3, ModeKind::FixedDuration, ModeKind::FixedOperations),
            "Tier 3 uses fixed_duration; remove mode or use tier = 2 for fixed_operations."
        );
        assert_eq!(
            tier_mode_mismatch_message(1, ModeKind::Micro, ModeKind::FixedDuration),
            "Tier 1 uses micro; remove mode or use tier = 3 for fixed_duration."
        );
    }
}
