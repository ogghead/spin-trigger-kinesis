use proc_macro::TokenStream;
use quote::quote;

const WIT_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../kinesis.wit");

#[proc_macro_attribute]
pub fn kinesis_component(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let func = syn::parse_macro_input!(item as syn::ItemFn);
    let func_name = &func.sig.ident;
    let await_postfix = func.sig.asyncness.map(|_| quote!(.await));
    let preamble = preamble();

    quote!(
        #func
        mod __spin_kinesis {
            mod preamble {
                #preamble
            }

            impl self::preamble::Guest for preamble::Kinesis {
                fn handle_batch_records(records: ::std::vec::Vec<::spin_kinesis_sdk::KinesisRecord>) -> ::std::result::Result<(), ::spin_kinesis_sdk::Error> {
                    ::spin_kinesis_sdk::executor::run(async move {
                        match super::#func_name(records)#await_postfix {
                            ::std::result::Result::Ok(()) => ::std::result::Result::Ok(()),
                            ::std::result::Result::Err(e) => {
                                eprintln!("{}", e);
                                ::std::result::Result::Err(::spin_kinesis_sdk::Error::Other(e.to_string()))
                            },
                        }
                    })
                }
            }
        }
    ).into()
}

fn preamble() -> proc_macro2::TokenStream {
    let world = "spin-kinesis";
    quote! {
        #![allow(missing_docs)]
        ::spin_kinesis_sdk::wit_bindgen::generate!({
            world: #world,
            path: #WIT_PATH,
            runtime_path: "::spin_kinesis_sdk::wit_bindgen::rt",
            exports: {
                world: Kinesis
            },
            with: {
                "fermyon:spin-kinesis/kinesis-types@2.0.0": ::spin_kinesis_sdk,
            }
        });
        pub struct Kinesis;
    }
}
