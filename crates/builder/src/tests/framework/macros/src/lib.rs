use proc_macro::TokenStream;
use quote::{quote, ToTokens};
use syn::{parse_macro_input, punctuated::Punctuated, ItemFn, Meta, Token};

struct TestConfig {
    args: Option<syn::Expr>,
    config: Option<syn::Expr>,
    multi_threaded: bool,
}

impl syn::parse::Parse for TestConfig {
    fn parse(input: syn::parse::ParseStream) -> syn::Result<Self> {
        let mut config = TestConfig { args: None, config: None, multi_threaded: false };

        if input.is_empty() {
            return Ok(config);
        }

        let metas: Punctuated<Meta, Token![,]> = input.parse_terminated(Meta::parse, Token![,])?;

        for meta in metas {
            match meta {
                Meta::Path(path) => {
                    if let Some(ident) = path.get_ident() {
                        let name = ident.to_string();
                        match name.as_str() {
                            "multi_threaded" => config.multi_threaded = true,
                            _ => {
                                return Err(syn::Error::new_spanned(
                                    path,
                                    format!(
                                        "Unknown attribute '{name}'. Use 'multi_threaded', 'args', or 'config'"
                                    ),
                                ));
                            }
                        }
                    }
                }
                Meta::NameValue(nv) => {
                    if let Some(ident) = nv.path.get_ident() {
                        let name = ident.to_string();
                        match name.as_str() {
                            "args" => config.args = Some(nv.value),
                            "config" => config.config = Some(nv.value),
                            _ => {
                                return Err(syn::Error::new_spanned(
                                    nv.path,
                                    format!(
                                        "Unknown attribute '{name}'. Use 'multi_threaded', 'args', or 'config'"
                                    ),
                                ));
                            }
                        }
                    }
                }
                _ => {
                    return Err(syn::Error::new_spanned(
                        meta,
                        "Invalid attribute format. Use 'multi_threaded', 'args', or 'config'",
                    ));
                }
            }
        }

        Ok(config)
    }
}

fn generate_instance_init(
    args: &Option<syn::Expr>,
    config: &Option<syn::Expr>,
) -> proc_macro2::TokenStream {
    match (args, config) {
        (None, None) => {
            quote! { crate::tests::LocalInstance::flashblocks().await? }
        }
        (Some(args_expr), None) => {
            quote! {
                crate::tests::LocalInstance::new::<crate::payload::FlashblocksBuilder>({
                    let mut args = #args_expr;
                    args.flashblocks.enabled = true;
                    args.flashblocks.flashblocks_port = crate::tests::get_available_port();
                    args.flashblocks.flashblocks_end_buffer_ms = 75;
                    args
                }).await?
            }
        }
        (None, Some(config_expr)) => {
            quote! {
                crate::tests::LocalInstance::new_with_config::<crate::payload::FlashblocksBuilder>({
                    let mut args = crate::args::OpRbuilderArgs::default();
                    args.flashblocks.enabled = true;
                    args.flashblocks.flashblocks_port = crate::tests::get_available_port();
                    args.flashblocks.flashblocks_end_buffer_ms = 75;
                    args
                }, #config_expr).await?
            }
        }
        (Some(args_expr), Some(config_expr)) => {
            quote! {
                crate::tests::LocalInstance::new_with_config::<crate::payload::FlashblocksBuilder>({
                    let mut args = #args_expr;
                    args.flashblocks.enabled = true;
                    args.flashblocks.flashblocks_port = crate::tests::get_available_port();
                    args.flashblocks.flashblocks_end_buffer_ms = 75;
                    args
                }, #config_expr).await?
            }
        }
    }
}

#[proc_macro_attribute]
pub fn rb_test(args: TokenStream, input: TokenStream) -> TokenStream {
    let input_fn = parse_macro_input!(input as ItemFn);
    let config = parse_macro_input!(args as TestConfig);

    validate_signature(&input_fn);

    let mut helper_fn = input_fn.clone();
    helper_fn.attrs.retain(|attr| !attr.path().is_ident("test") && !attr.path().is_ident("tokio"));

    let original_name = &input_fn.sig.ident;
    let test_name = syn::Ident::new(&format!("{original_name}_flashblocks"), original_name.span());
    let instance_init = generate_instance_init(&config.args, &config.config);

    let test_attribute = if config.multi_threaded {
        quote! { #[tokio::test(flavor = "multi_thread")] }
    } else {
        quote! { #[tokio::test] }
    };

    TokenStream::from(quote! {
        #helper_fn

        #test_attribute
        async fn #test_name() -> eyre::Result<()> {
            let subscriber = tracing_subscriber::fmt()
                .with_env_filter(std::env::var("RUST_LOG")
                    .unwrap_or_else(|_| "info".to_string()))
                .with_file(true)
                .with_line_number(true)
                .with_test_writer()
                .finish();
            let _guard = tracing::subscriber::set_global_default(subscriber);
            tracing::info!("{} start", stringify!(#test_name));

            let instance = #instance_init;
            #original_name(instance).await
        }
    })
}

fn validate_signature(item_fn: &ItemFn) {
    if item_fn.sig.asyncness.is_none() {
        panic!("Function must be async.");
    }
    if item_fn.sig.inputs.len() != 1 {
        panic!("Function must have exactly one parameter of type LocalInstance.");
    }

    let output_types = item_fn.sig.output.to_token_stream().to_string().replace(" ", "");

    if output_types != "->eyre::Result<()>" {
        panic!("Function must return Result<(), eyre::Error>. Actual: {output_types}",);
    }
}
