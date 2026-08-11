//! Proc macros for cntryl-stress.

use proc_macro::TokenStream;
use proc_macro2::{Span, TokenStream as TokenStream2};
use proc_macro_crate::{crate_name, FoundCrate};
use quote::quote;
use std::collections::HashSet;
use syn::parse::Parser;
use syn::{
    parse_macro_input, Expr, ExprLit, FnArg, ItemFn, Lit, Meta, MetaNameValue, Signature, Token,
    Type,
};

const MAX_TIER: u32 = 6;

/// Mark a function as a stress benchmark.
///
/// Benchmark functions take exactly one `&mut StressContext`, may be synchronous
/// or asynchronous, and return `()`, `Result<(), E>`, or `StressResult`.
///
/// Common usage stays intentionally small:
///
/// ```rust,ignore
/// # use stress_alias as cntryl_stress;
/// use cntryl_stress::{black_box, stress, StressContext};
///
/// #[stress(tier = 1)]
/// fn parse_hot_path(ctx: &mut StressContext) {
///     let header = b"content-type:application/json";
///     ctx.measure("colon lookup", || black_box(header.iter().position(|byte| *byte == b':')));
/// }
///
/// #[stress(tier = 2)]
/// fn write_batch(ctx: &mut StressContext) {
///     ctx.measure("write batch", || black_box([1_u8, 2, 3]));
/// }
/// ```
///
/// Supported attributes:
///
/// - `tier = 1` through `6` (defaults to `2`)
/// - `name = "custom_name"`
/// - `ignore`
/// - `role = "gate"`, `"diagnostic"`, or `"experimental"`
/// - `max_ns_per_op = 1000`
/// - `max_allocs_per_op = 0`
/// - `max_bytes_per_op = 0`
/// - `max_regression_pct = 5`
/// - `max_rsd_pct = 10`
/// - `metadata(owner = "storage", scenario = "fanout")`
#[proc_macro_attribute]
pub fn stress(attr: TokenStream, item: TokenStream) -> TokenStream {
    let input = parse_macro_input!(item as ItemFn);
    let fn_name = &input.sig.ident;
    let fn_name_str = fn_name.to_string();
    let is_async = input.sig.asyncness.is_some();

    let attrs = match StressAttrs::parse(attr.into()) {
        Ok(attrs) => attrs,
        Err(error) => return error.to_compile_error().into(),
    };
    if let Err(error) = validate_stress_signature(&input.sig) {
        return error.to_compile_error().into();
    }
    if let Some(error) = tier_error(attrs.tier) {
        return syn::Error::new_spanned(fn_name, error)
            .to_compile_error()
            .into();
    }
    let stress_crate = match stress_crate_path() {
        Ok(path) => path,
        Err(error) => return error.to_compile_error().into(),
    };

    let benchmark_name = attrs.name.unwrap_or_else(|| fn_name_str.clone());
    let is_ignored = attrs.ignore;
    let tier = attrs.tier;
    let Some(derived_mode) = mode_for_tier(tier) else {
        return syn::Error::new_spanned(fn_name, "cntryl-stress tiers are 1 through 6")
            .to_compile_error()
            .into();
    };
    let mode_kind = derived_mode;
    let mode = mode_kind.tokens(&stress_crate);
    let max_ns_per_op = option_f64_tokens(attrs.budgets.ns_per_op);
    let max_allocs_per_op = option_f64_tokens(attrs.budgets.allocs_per_op);
    let max_bytes_per_op = option_f64_tokens(attrs.budgets.bytes_per_op);
    let max_regression_pct = option_f64_tokens(attrs.budgets.regression_pct);
    let max_rsd_pct = option_f64_tokens(attrs.budgets.rsd_pct);
    let mut metadata = attrs.metadata;
    if let Some(role) = attrs.role {
        metadata.push(("trust_class".to_string(), role));
    }
    let metadata_keys = metadata.iter().map(|(key, _)| key);
    let metadata_values = metadata.iter().map(|(_, value)| value);
    let submit_ident = syn::Ident::new(
        &format!("__STRESS_BENCH_{}", fn_name_str.to_uppercase()),
        fn_name.span(),
    );
    let wrapper_ident = syn::Ident::new(&format!("__stress_wrapper_{fn_name_str}"), fn_name.span());
    let invocation = if is_async {
        quote! {
            #stress_crate::__private::block_on(#fn_name(ctx))
        }
    } else {
        quote! { #fn_name(ctx) }
    };
    let wrapper = quote! {
        fn #wrapper_ident(ctx: &mut #stress_crate::StressContext) -> #stress_crate::StressResult {
            #stress_crate::__private::IntoStressResult::into_stress_result(#invocation)
        }
    };

    quote! {
        #input
        #wrapper

        #[allow(non_upper_case_globals)]
        #[#stress_crate::__private::linkme::distributed_slice(#stress_crate::__private::STRESS_BENCHMARKS)]
        #[linkme(crate = #stress_crate::__private::linkme)]
        static #submit_ident: #stress_crate::__private::BenchmarkEntry = #stress_crate::__private::BenchmarkEntry {
            name: #benchmark_name,
            function_name: #fn_name_str,
            func: #wrapper_ident,
            ignored: #is_ignored,
            module_path: module_path!(),
            tier: #tier,
            mode: #mode,
            budgets: #stress_crate::artifact::BenchmarkBudgets {
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
    ignore: bool,
    role: Option<String>,
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
            ignore: false,
            role: None,
            budgets: StressBudgets::default(),
            metadata: Vec::new(),
        }
    }
}

impl StressAttrs {
    fn parse(attr: proc_macro2::TokenStream) -> syn::Result<Self> {
        let parser = syn::punctuated::Punctuated::<Meta, Token![,]>::parse_terminated;
        let metas = parser.parse2(attr)?;
        let mut attrs = Self::default();
        let mut singleton_attributes = HashSet::new();

        for meta in metas {
            match meta {
                Meta::Path(path) if path.is_ident("ignore") => {
                    mark_singleton(&mut singleton_attributes, "ignore", &path)?;
                    attrs.ignore = true;
                }
                Meta::NameValue(name_value) if name_value.path.is_ident("name") => {
                    mark_singleton(&mut singleton_attributes, "name", &name_value)?;
                    let name = string_value(&name_value)?;
                    if name.trim().is_empty() {
                        return Err(syn::Error::new_spanned(
                            &name_value.value,
                            "stress benchmark name must not be empty or whitespace",
                        ));
                    }
                    attrs.name = Some(name);
                }
                Meta::NameValue(name_value) if name_value.path.is_ident("tier") => {
                    mark_singleton(&mut singleton_attributes, "tier", &name_value)?;
                    let tier = int_value(&name_value)?;
                    if let Some(error) = tier_error(tier) {
                        return Err(syn::Error::new_spanned(name_value, error));
                    }
                    attrs.tier = tier;
                }
                Meta::NameValue(name_value) if name_value.path.is_ident("role") => {
                    mark_singleton(&mut singleton_attributes, "role", &name_value)?;
                    attrs.role = Some(role_value(&name_value)?);
                }
                Meta::NameValue(name_value) if name_value.path.is_ident("mode") => {
                    return Err(syn::Error::new_spanned(
                        name_value,
                        "mode is not a public stress attribute; choose tier = 1 through 6",
                    ));
                }
                Meta::NameValue(name_value) if name_value.path.is_ident("max_ns_per_op") => {
                    mark_singleton(&mut singleton_attributes, "max_ns_per_op", &name_value)?;
                    attrs.budgets.ns_per_op = Some(nonnegative_budget_value(&name_value)?);
                }
                Meta::NameValue(name_value) if name_value.path.is_ident("max_allocs_per_op") => {
                    mark_singleton(&mut singleton_attributes, "max_allocs_per_op", &name_value)?;
                    attrs.budgets.allocs_per_op = Some(nonnegative_budget_value(&name_value)?);
                }
                Meta::NameValue(name_value) if name_value.path.is_ident("max_bytes_per_op") => {
                    mark_singleton(&mut singleton_attributes, "max_bytes_per_op", &name_value)?;
                    attrs.budgets.bytes_per_op = Some(nonnegative_budget_value(&name_value)?);
                }
                Meta::NameValue(name_value) if name_value.path.is_ident("max_regression_pct") => {
                    mark_singleton(&mut singleton_attributes, "max_regression_pct", &name_value)?;
                    attrs.budgets.regression_pct = Some(percentage_budget_value(&name_value)?);
                }
                Meta::NameValue(name_value) if name_value.path.is_ident("max_rsd_pct") => {
                    mark_singleton(&mut singleton_attributes, "max_rsd_pct", &name_value)?;
                    attrs.budgets.rsd_pct = Some(nonnegative_budget_value(&name_value)?);
                }
                Meta::List(list) if list.path.is_ident("metadata") => {
                    let metadata = parse_metadata(list.tokens.clone())?;
                    for (key, value) in metadata {
                        if key == "trust_class" {
                            return Err(syn::Error::new_spanned(
                                &list,
                                "trust_class is reserved; use role = \"diagnostic\" or role = \"experimental\"",
                            ));
                        }
                        if attrs
                            .metadata
                            .iter()
                            .any(|(existing_key, _)| existing_key == &key)
                        {
                            return Err(syn::Error::new_spanned(
                                &list,
                                format!("stress metadata key `{key}` may be specified only once"),
                            ));
                        }
                        attrs.metadata.push((key, value));
                    }
                }
                other => {
                    return Err(syn::Error::new_spanned(
                        other,
                        "unsupported stress attribute; expected tier, name, ignore, role, a max_* budget, or metadata(...)"
                    ));
                }
            }
        }

        Ok(attrs)
    }
}

fn mark_singleton(
    seen: &mut HashSet<&'static str>,
    name: &'static str,
    tokens: impl quote::ToTokens,
) -> syn::Result<()> {
    if seen.insert(name) {
        Ok(())
    } else {
        Err(syn::Error::new_spanned(
            tokens,
            format!("stress attribute `{name}` may be specified only once"),
        ))
    }
}

fn role_value(name_value: &MetaNameValue) -> syn::Result<String> {
    let value = string_value(name_value)?;
    match value.as_str() {
        "gate" | "diagnostic" | "experimental" => Ok(value),
        _ => Err(syn::Error::new_spanned(
            &name_value.value,
            "stress role must be \"gate\", \"diagnostic\", or \"experimental\"",
        )),
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

fn nonnegative_budget_value(name_value: &MetaNameValue) -> syn::Result<f64> {
    let value = float_value(name_value)?;
    if value.is_finite() && value >= 0.0 {
        Ok(value)
    } else {
        Err(syn::Error::new_spanned(
            &name_value.value,
            "stress budgets must be finite non-negative numbers",
        ))
    }
}

fn percentage_budget_value(name_value: &MetaNameValue) -> syn::Result<f64> {
    let value = nonnegative_budget_value(name_value)?;
    if value <= 100.0 {
        Ok(value)
    } else {
        Err(syn::Error::new_spanned(
            &name_value.value,
            "max_regression_pct must be between 0 and 100",
        ))
    }
}

fn option_f64_tokens(value: Option<f64>) -> TokenStream2 {
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
    fn tokens(self, stress_crate: &TokenStream2) -> TokenStream2 {
        match self {
            Self::Micro => quote! { #stress_crate::artifact::BenchmarkModeKind::Micro },
            Self::FixedOperations => {
                quote! { #stress_crate::artifact::BenchmarkModeKind::FixedOperations }
            }
            Self::FixedDuration => {
                quote! { #stress_crate::artifact::BenchmarkModeKind::FixedDuration }
            }
        }
    }
}

fn validate_stress_signature(signature: &Signature) -> syn::Result<()> {
    if let Some(unsafety) = &signature.unsafety {
        return Err(syn::Error::new_spanned(
            unsafety,
            "#[stress] benchmark functions cannot be unsafe",
        ));
    }
    if let Some(abi) = &signature.abi {
        return Err(syn::Error::new_spanned(
            abi,
            "#[stress] benchmark functions cannot use an extern ABI",
        ));
    }
    if !signature.generics.params.is_empty() || signature.generics.where_clause.is_some() {
        return Err(syn::Error::new_spanned(
            &signature.generics,
            "#[stress] benchmark functions cannot be generic",
        ));
    }
    if let Some(variadic) = &signature.variadic {
        return Err(syn::Error::new_spanned(
            variadic,
            "#[stress] benchmark functions cannot be variadic",
        ));
    }
    if let Some(FnArg::Receiver(receiver)) = signature
        .inputs
        .iter()
        .find(|parameter| matches!(parameter, FnArg::Receiver(_)))
    {
        return Err(syn::Error::new_spanned(
            receiver,
            "#[stress] benchmark functions cannot have a self receiver; use a free function with &mut StressContext",
        ));
    }
    if signature.inputs.len() != 1 {
        return Err(syn::Error::new_spanned(
            &signature.inputs,
            "#[stress] benchmark functions require exactly one parameter: &mut StressContext",
        ));
    }

    let parameter = signature
        .inputs
        .first()
        .expect("one benchmark parameter was checked above");
    let FnArg::Typed(parameter) = parameter else {
        unreachable!("self receivers were rejected above");
    };
    if !is_mut_stress_context(&parameter.ty) {
        return Err(syn::Error::new_spanned(
            &parameter.ty,
            "#[stress] benchmark parameter must have type &mut StressContext",
        ));
    }
    Ok(())
}

fn is_mut_stress_context(ty: &Type) -> bool {
    let Type::Reference(reference) = ty else {
        return false;
    };
    if reference.mutability.is_none() || reference.lifetime.is_some() {
        return false;
    }
    let Type::Path(path) = reference.elem.as_ref() else {
        return false;
    };
    path.qself.is_none()
        && path
            .path
            .segments
            .last()
            .is_some_and(|segment| segment.ident == "StressContext" && segment.arguments.is_empty())
}

fn stress_crate_path() -> syn::Result<TokenStream2> {
    match crate_name("cntryl-stress") {
        Ok(FoundCrate::Itself) => Ok(quote! { ::cntryl_stress }),
        Ok(FoundCrate::Name(name)) => {
            let ident = syn::Ident::new(&name, Span::call_site());
            Ok(quote! { ::#ident })
        }
        Err(error) => Err(syn::Error::new(
            Span::call_site(),
            format!(
                "could not find the cntryl-stress dependency: {error}; add cntryl-stress to Cargo.toml"
            ),
        )),
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
    let stress_crate = match stress_crate_path() {
        Ok(path) => path,
        Err(error) => return error.to_compile_error().into(),
    };
    quote! {
        fn main() {
            #stress_crate::__private::stress_binary_main();
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
    fn mode_attribute_is_rejected() {
        let error = StressAttrs::parse(quote::quote! { tier = 1, mode = "micro" })
            .expect_err("mode is not public");

        assert!(error
            .to_string()
            .contains("mode is not a public stress attribute"));
    }
}
