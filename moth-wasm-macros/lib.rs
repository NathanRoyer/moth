use proc_macro::{TokenStream};
use syn::{parse_macro_input, ItemFn, Ident};
use quote::{quote, format_ident};

#[proc_macro_attribute]
pub fn moth_callback(args: TokenStream, input: TokenStream) -> TokenStream {
    assert!(args.is_empty(), "This macro attribute has no argument");

    let mut func = parse_macro_input!(input as ItemFn);

    let orig_span = func.sig.ident.span();
    let orig_name = core::mem::replace(&mut func.sig.ident, Ident::new("callback", orig_span));

    let mut ptrs = Vec::new();
    let mut lens = Vec::new();
    for arg in 0..(func.sig.inputs.len().checked_sub(1).expect("Missing Request parameter")) {
        ptrs.push(format_ident!("p{}_ptr", arg));
        lens.push(format_ident!("p{}_len", arg));
    }

    quote! {
        #[no_mangle]
        extern "C" fn #orig_name(req_ptr: u64, req_token: u64, #(#ptrs: u64, #lens: u64)*) -> u64 {
            #func

            let mut request = unsafe { moth_wasm::Request::new(req_token, req_ptr) };

            let ret: Option<Box<moth_wasm::lmfu::json::JsonFile>>;
            ret = callback(request, #(moth_wasm::param(#ptrs, #lens),)*);

            match ret {
                Some(json) => Box::into_raw(json) as _,
                None => 0,
            }
        }
    }.into()
}