//! Proc macros for cntryl-stress benchmark framework.
//!
//! This crate provides the `#[stress_test]` attribute macro for defining
//! benchmarks that are automatically discovered and run.

use proc_macro::TokenStream;
use quote::quote;
use syn::{parse_macro_input, ItemFn};

/// Mark a function as a stress benchmark.
///
/// Functions annotated with `#[stress_test]` are automatically discovered
/// and run when using `cargo stress` or the `stress_main!` macro.
///
/// # Example
///
/// ```rust,ignore
/// use cntryl_stress::{stress_test, BenchContext};
///
/// #[stress_test]
/// fn my_benchmark(ctx: &mut BenchContext) {
///     let data = vec![0u8; 1024 * 1024];
///     ctx.set_bytes(data.len() as u64);
///     ctx.measure(|| {
///         std::hint::black_box(&data);
///     });
/// }
/// ```
///
/// # Attributes
///
/// - `#[stress_test]` - Basic benchmark
/// - `#[stress_test(ignore)]` - Skip this benchmark unless explicitly requested
/// - `#[stress_test(name = "custom_name")]` - Use a custom name instead of function name
#[proc_macro_attribute]
pub fn stress_test(attr: TokenStream, item: TokenStream) -> TokenStream {
    let input = parse_macro_input!(item as ItemFn);
    let fn_name = &input.sig.ident;
    let fn_name_str = fn_name.to_string();

    // Parse attributes
    let attr_str = attr.to_string();
    let is_ignored = attr_str.contains("ignore");
    let custom_name = parse_custom_name(&attr_str).unwrap_or_else(|| fn_name_str.clone());

    // Generate a unique identifier for the inventory submission
    let submit_ident = syn::Ident::new(
        &format!("__STRESS_BENCH_{}", fn_name_str.to_uppercase()),
        fn_name.span(),
    );

    let expanded = quote! {
        #input

        #[allow(non_upper_case_globals)]
        #[::cntryl_stress::__private::linkme::distributed_slice(::cntryl_stress::__private::STRESS_BENCHMARKS)]
        #[linkme(crate = ::cntryl_stress::__private::linkme)]
        static #submit_ident: ::cntryl_stress::__private::BenchmarkEntry = ::cntryl_stress::__private::BenchmarkEntry {
            name: #custom_name,
            func: #fn_name,
            ignored: #is_ignored,
            module_path: module_path!(),
        };
    };

    TokenStream::from(expanded)
}

fn parse_custom_name(attr: &str) -> Option<String> {
    // Simple parsing for name = "value"
    if let Some(start) = attr.find("name") {
        let rest = &attr[start..];
        if let Some(eq) = rest.find('=') {
            let after_eq = &rest[eq + 1..];
            if let Some(quote_start) = after_eq.find('"') {
                let after_quote = &after_eq[quote_start + 1..];
                if let Some(quote_end) = after_quote.find('"') {
                    return Some(after_quote[..quote_end].to_string());
                }
            }
        }
    }
    None
}

/// Generate the main function for running stress benchmarks.
///
/// Place this at the end of your benchmark file to create an executable
/// that discovers and runs all `#[stress_test]` benchmarks.
///
/// # Example
///
/// ```rust,ignore
/// use cntryl_stress::{stress_test, stress_main, BenchContext};
///
/// #[stress_test]
/// fn benchmark_one(ctx: &mut BenchContext) {
///     ctx.measure(|| { /* ... */ });
/// }
///
/// stress_main!();
/// ```
#[proc_macro]
pub fn stress_main(_input: TokenStream) -> TokenStream {
    let expanded = quote! {
        fn main() {
            ::cntryl_stress::run_registered_benchmarks();
        }
    };
    TokenStream::from(expanded)
}
