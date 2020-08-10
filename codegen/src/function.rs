#![allow(unused)]

use std::collections::HashMap;

use quote::{quote, quote_spanned};
use syn::{parse::Parse, parse::ParseStream, parse::Parser, spanned::Spanned};

#[derive(Debug, Default)]
pub(crate) struct ExportedFnParams {
    pub name: Option<String>,
    pub return_raw: bool,
}

impl Parse for ExportedFnParams {
    fn parse(args: ParseStream) -> syn::Result<Self> {
        if args.is_empty() {
            return Ok(ExportedFnParams::default());
        }

        let arg_list = args.call(
            syn::punctuated::Punctuated::<syn::Expr, syn::Token![,]>::parse_separated_nonempty,
        )?;

        let mut attrs: HashMap<proc_macro2::Ident, Option<syn::LitStr>> = HashMap::new();
        for arg in arg_list {
            let (left, right) = match arg {
                syn::Expr::Assign(syn::ExprAssign {
                    ref left,
                    ref right,
                    ..
                }) => {
                    let attr_name: syn::Ident = match left.as_ref() {
                        syn::Expr::Path(syn::ExprPath {
                            path: attr_path, ..
                        }) => attr_path.get_ident().cloned().ok_or_else(|| {
                            syn::Error::new(attr_path.span(), "expecting attribute name")
                        })?,
                        x => return Err(syn::Error::new(x.span(), "expecting attribute name")),
                    };
                    let attr_value = match right.as_ref() {
                        syn::Expr::Lit(syn::ExprLit {
                            lit: syn::Lit::Str(string),
                            ..
                        }) => string.clone(),
                        x => return Err(syn::Error::new(x.span(), "expecting string literal")),
                    };
                    (attr_name, Some(attr_value))
                }
                syn::Expr::Path(syn::ExprPath {
                    path: attr_path, ..
                }) => attr_path
                    .get_ident()
                    .cloned()
                    .map(|a| (a, None))
                    .ok_or_else(|| syn::Error::new(attr_path.span(), "expecting attribute name"))?,
                x => return Err(syn::Error::new(x.span(), "expecting identifier")),
            };
            attrs.insert(left, right);
        }

        let mut name = None;
        let mut return_raw = false;
        for (ident, value) in attrs.drain() {
            match (ident.to_string().as_ref(), value) {
                ("name", Some(s)) => name = Some(s.value()),
                ("name", None) => return Err(syn::Error::new(ident.span(), "requires value")),
                ("return_raw", None) => return_raw = true,
                ("return_raw", Some(s)) => {
                    return Err(syn::Error::new(s.span(), "extraneous value"))
                }
                (attr, _) => {
                    return Err(syn::Error::new(
                        ident.span(),
                        format!("unknown attribute '{}'", attr),
                    ))
                }
            }
        }

        Ok(ExportedFnParams { name, return_raw })
    }
}

#[derive(Debug)]
pub(crate) struct ExportedFn {
    entire_span: proc_macro2::Span,
    signature: syn::Signature,
    is_public: bool,
    mut_receiver: bool,
    params: ExportedFnParams,
}

impl Parse for ExportedFn {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let fn_all: syn::ItemFn = input.parse()?;
        let entire_span = fn_all.span();
        let str_type_path = syn::parse2::<syn::Path>(quote! { str }).unwrap();

        // Determine if the function is public.
        let is_public = match fn_all.vis {
            syn::Visibility::Public(_) => true,
            _ => false,
        };
        // Determine whether function generates a special calling convention for a mutable
        // reciever.
        let mut_receiver = {
            if let Some(first_arg) = fn_all.sig.inputs.first() {
                match first_arg {
                    syn::FnArg::Receiver(syn::Receiver {
                        reference: Some(_), ..
                    }) => true,
                    syn::FnArg::Typed(syn::PatType { ref ty, .. }) => match ty.as_ref() {
                        &syn::Type::Reference(syn::TypeReference {
                            mutability: Some(_),
                            ..
                        }) => true,
                        &syn::Type::Reference(syn::TypeReference {
                            mutability: None,
                            ref elem,
                            ..
                        }) => match elem.as_ref() {
                            &syn::Type::Path(ref p) if p.path == str_type_path => false,
                            _ => {
                                return Err(syn::Error::new(
                                    ty.span(),
                                    "references from Rhai in this position \
                                            must be mutable",
                                ))
                            }
                        },
                        _ => false,
                    },
                    _ => false,
                }
            } else {
                false
            }
        };

        // All arguments after the first must be moved except for &str.
        for arg in fn_all.sig.inputs.iter().skip(1) {
            let ty = match arg {
                syn::FnArg::Typed(syn::PatType { ref ty, .. }) => ty,
                _ => panic!("internal error: receiver argument outside of first position!?"),
            };
            let is_ok = match ty.as_ref() {
                &syn::Type::Reference(syn::TypeReference {
                    mutability: Some(_),
                    ..
                }) => false,
                &syn::Type::Reference(syn::TypeReference {
                    mutability: None,
                    ref elem,
                    ..
                }) => match elem.as_ref() {
                    &syn::Type::Path(ref p) if p.path == str_type_path => true,
                    _ => false,
                },
                &syn::Type::Verbatim(_) => false,
                _ => true,
            };
            if !is_ok {
                return Err(syn::Error::new(
                    ty.span(),
                    "this type in this position passes from \
                                                        Rhai by value",
                ));
            }
        }

        // No returning references or pointers.
        if let syn::ReturnType::Type(_, ref rtype) = fn_all.sig.output {
            match rtype.as_ref() {
                &syn::Type::Ptr(_) => {
                    return Err(syn::Error::new(
                        fn_all.sig.output.span(),
                        "cannot return a pointer to Rhai",
                    ))
                }
                &syn::Type::Reference(_) => {
                    return Err(syn::Error::new(
                        fn_all.sig.output.span(),
                        "cannot return a reference to Rhai",
                    ))
                }
                _ => {}
            }
        }
        Ok(ExportedFn {
            entire_span,
            signature: fn_all.sig,
            is_public,
            mut_receiver,
            params: ExportedFnParams::default(),
        })
    }
}

impl ExportedFn {
    pub(crate) fn mutable_receiver(&self) -> bool {
        self.mut_receiver
    }

    pub(crate) fn is_public(&self) -> bool {
        self.is_public
    }

    pub(crate) fn span(&self) -> &proc_macro2::Span {
        &self.entire_span
    }

    pub(crate) fn name(&self) -> &syn::Ident {
        &self.signature.ident
    }

    pub(crate) fn arg_list(&self) -> impl Iterator<Item = &syn::FnArg> {
        self.signature.inputs.iter()
    }

    pub(crate) fn arg_count(&self) -> usize {
        self.signature.inputs.len()
    }

    pub(crate) fn return_type(&self) -> Option<&syn::Type> {
        if let syn::ReturnType::Type(_, ref rtype) = self.signature.output {
            Some(rtype)
        } else {
            None
        }
    }

    pub fn generate_with_params(mut self,
                                mut params: ExportedFnParams) -> proc_macro2::TokenStream {
        self.params = params;
        self.generate()
    }

    pub fn generate(self) -> proc_macro2::TokenStream {
        let name_str = if let Some(ref name) = self.params.name {
            name.clone()
        } else {
            self.name().to_string()
        };
        let name: syn::Ident = syn::Ident::new(&format!("rhai_fn_{}", name_str),
                                               self.name().span());
        let impl_block = self.generate_impl("Token");
        let callable_block = self.generate_callable("Token");
        let input_types_block = self.generate_input_types("Token");
        quote! {
            #[allow(unused)]
            pub mod #name {
                use super::*;
                struct Token();
                #impl_block
                #callable_block
                #input_types_block
            }
        }
    }

    pub fn generate_callable(&self, on_type_name: &str) -> proc_macro2::TokenStream {
        let token_name: syn::Ident = syn::Ident::new(on_type_name, self.name().span());
        let callable_fn_name: syn::Ident = syn::Ident::new(
            format!("{}_callable", on_type_name.to_lowercase()).as_str(),
            self.name().span(),
        );
        quote! {
            pub fn #callable_fn_name() -> CallableFunction {
                CallableFunction::from_plugin(#token_name())
            }
        }
    }

    pub fn generate_input_types(&self, on_type_name: &str) -> proc_macro2::TokenStream {
        let token_name: syn::Ident = syn::Ident::new(on_type_name, self.name().span());
        let input_types_fn_name: syn::Ident = syn::Ident::new(
            format!("{}_input_types", on_type_name.to_lowercase()).as_str(),
            self.name().span(),
        );
        quote! {
            pub fn #input_types_fn_name() -> Box<[std::any::TypeId]> {
                #token_name().input_types()
            }
        }
    }

    pub fn generate_impl(&self, on_type_name: &str) -> proc_macro2::TokenStream {
        let name: syn::Ident = if let Some(ref name) = self.params.name {
            syn::Ident::new(name, self.name().span())
        } else {
            self.name().clone()
        };

        let arg_count = self.arg_count();
        let is_method_call = self.mutable_receiver();

        let mut unpack_stmts: Vec<syn::Stmt> = Vec::new();
        let mut unpack_exprs: Vec<syn::Expr> = Vec::new();
        let mut input_type_exprs: Vec<syn::Expr> = Vec::new();
        let skip_first_arg;

        // Handle the first argument separately if the function has a "method like" receiver
        if is_method_call {
            skip_first_arg = true;
            let first_arg = self.arg_list().next().unwrap();
            let var = syn::Ident::new("arg0", proc_macro2::Span::call_site());
            match first_arg {
                syn::FnArg::Typed(pattern) => {
                    let arg_type: &syn::Type = {
                        match pattern.ty.as_ref() {
                            &syn::Type::Reference(syn::TypeReference { ref elem, .. }) => {
                                elem.as_ref()
                            }
                            ref p => p,
                        }
                    };
                    let downcast_span = quote_spanned!(
                        arg_type.span()=> &mut args[0usize].write_lock::<#arg_type>().unwrap());
                    unpack_stmts.push(
                        syn::parse2::<syn::Stmt>(quote! {
                            let #var: &mut _ = #downcast_span;
                        })
                        .unwrap(),
                    );
                    input_type_exprs.push(
                        syn::parse2::<syn::Expr>(quote_spanned!(
                            arg_type.span()=> std::any::TypeId::of::<#arg_type>()
                        ))
                        .unwrap(),
                    );
                }
                syn::FnArg::Receiver(_) => todo!("true self parameters not implemented yet"),
            }
            unpack_exprs.push(syn::parse2::<syn::Expr>(quote! { #var }).unwrap());
        } else {
            skip_first_arg = false;
        }

        // Handle the rest of the arguments, which all are passed by value.
        //
        // The only exception is strings, which need to be downcast to ImmutableString to enable a
        // zero-copy conversion to &str by reference.
        let str_type_path = syn::parse2::<syn::Path>(quote! { str }).unwrap();
        for (i, arg) in self.arg_list().enumerate().skip(skip_first_arg as usize) {
            let var = syn::Ident::new(&format!("arg{}", i), proc_macro2::Span::call_site());
            let is_str_ref;
            match arg {
                syn::FnArg::Typed(pattern) => {
                    let arg_type: &syn::Type = pattern.ty.as_ref();
                    let downcast_span = match pattern.ty.as_ref() {
                        &syn::Type::Reference(syn::TypeReference {
                            mutability: None,
                            ref elem,
                            ..
                        }) => match elem.as_ref() {
                            &syn::Type::Path(ref p) if p.path == str_type_path => {
                                is_str_ref = true;
                                quote_spanned!(arg_type.span()=>
                                                   args[#i]
                                                   .downcast_clone::<ImmutableString>()
                                                   .unwrap())
                            }
                            _ => panic!("internal error: why wasn't this found earlier!?"),
                        },
                        _ => {
                            is_str_ref = false;
                            quote_spanned!(arg_type.span()=>
                                            args[#i].downcast_clone::<#arg_type>().unwrap())
                        }
                    };

                    unpack_stmts.push(
                        syn::parse2::<syn::Stmt>(quote! {
                            let #var = #downcast_span;
                        })
                        .unwrap(),
                    );
                    if !is_str_ref {
                        input_type_exprs.push(
                            syn::parse2::<syn::Expr>(quote_spanned!(
                                arg_type.span()=> std::any::TypeId::of::<#arg_type>()
                            ))
                            .unwrap(),
                        );
                    } else {
                        input_type_exprs.push(
                            syn::parse2::<syn::Expr>(quote_spanned!(
                                arg_type.span()=> std::any::TypeId::of::<ImmutableString>()
                            ))
                            .unwrap(),
                        );
                    }
                }
                syn::FnArg::Receiver(_) => panic!("internal error: how did this happen!?"),
            }
            if !is_str_ref {
                unpack_exprs.push(syn::parse2::<syn::Expr>(quote! { #var }).unwrap());
            } else {
                unpack_exprs.push(syn::parse2::<syn::Expr>(quote! { &#var }).unwrap());
            }
        }

        // In method calls, the first argument will need to be mutably borrowed. Because Rust marks
        // that as needing to borrow the entire array, all of the previous argument unpacking via
        // clone needs to happen first.
        if is_method_call {
            let arg0 = unpack_stmts.remove(0);
            unpack_stmts.push(arg0);
        }

        // Handle "raw returns", aka cases where the result is a dynamic or an error.
        //
        // This allows skipping the Dynamic::from wrap.
        let return_expr = if !self.params.return_raw {
            quote! {
                Ok(Dynamic::from(#name(#(#unpack_exprs),*)))
            }
        } else {
            quote! {
                #name(#(#unpack_exprs),*)
            }
        };

        let type_name = syn::Ident::new(on_type_name, proc_macro2::Span::call_site());
        quote! {
            impl PluginFunction for #type_name {
                fn call(&self,
                        args: &mut [&mut Dynamic], pos: Position
                ) -> Result<Dynamic, Box<EvalAltResult>> {
                    if args.len() != #arg_count {
                        return Err(Box::new(EvalAltResult::ErrorRuntime(
                                format!("wrong arg count: {} != {}",
                                        args.len(), #arg_count), Position::none())));
                    }
                    #(#unpack_stmts)*
                    #return_expr
                }

                fn is_method_call(&self) -> bool { #is_method_call }
                fn is_varadic(&self) -> bool { false }
                fn clone_boxed(&self) -> Box<dyn PluginFunction> { Box::new(#type_name()) }
                fn input_types(&self) -> Box<[std::any::TypeId]> {
                    vec![#(#input_type_exprs),*].into_boxed_slice()
                }
            }
        }
    }
}

#[cfg(test)]
mod function_tests {
    use super::ExportedFn;

    use proc_macro2::TokenStream;
    use quote::quote;

    #[test]
    fn minimal_fn() {
        let input_tokens: TokenStream = quote! {
            pub fn do_nothing() { }
        };

        let item_fn = syn::parse2::<ExportedFn>(input_tokens).unwrap();
        assert_eq!(&item_fn.name().to_string(), "do_nothing");
        assert!(!item_fn.mutable_receiver());
        assert!(item_fn.is_public());
        assert!(item_fn.return_type().is_none());
        assert_eq!(item_fn.arg_list().count(), 0);
    }

    #[test]
    fn one_arg_fn() {
        let input_tokens: TokenStream = quote! {
            pub fn do_something(x: usize) { }
        };

        let item_fn = syn::parse2::<ExportedFn>(input_tokens).unwrap();
        assert_eq!(&item_fn.name().to_string(), "do_something");
        assert_eq!(item_fn.arg_list().count(), 1);
        assert!(!item_fn.mutable_receiver());
        assert!(item_fn.is_public());
        assert!(item_fn.return_type().is_none());

        assert_eq!(
            item_fn.arg_list().next().unwrap(),
            &syn::parse2::<syn::FnArg>(quote! { x: usize }).unwrap()
        );
    }

    #[test]
    fn two_arg_fn() {
        let input_tokens: TokenStream = quote! {
            pub fn do_something(x: usize, y: f32) { }
        };

        let item_fn = syn::parse2::<ExportedFn>(input_tokens).unwrap();
        assert_eq!(&item_fn.name().to_string(), "do_something");
        assert_eq!(item_fn.arg_list().count(), 2);
        assert!(!item_fn.mutable_receiver());
        assert!(item_fn.is_public());
        assert!(item_fn.return_type().is_none());

        assert_eq!(
            item_fn.arg_list().next().unwrap(),
            &syn::parse2::<syn::FnArg>(quote! { x: usize }).unwrap()
        );
        assert_eq!(
            item_fn.arg_list().skip(1).next().unwrap(),
            &syn::parse2::<syn::FnArg>(quote! { y: f32 }).unwrap()
        );
    }

    #[test]
    fn usize_returning_fn() {
        let input_tokens: TokenStream = quote! {
            pub fn get_magic_number() -> usize { 42 }
        };

        let item_fn = syn::parse2::<ExportedFn>(input_tokens).unwrap();
        assert_eq!(&item_fn.name().to_string(), "get_magic_number");
        assert!(!item_fn.mutable_receiver());
        assert!(item_fn.is_public());
        assert_eq!(item_fn.arg_list().count(), 0);
        assert_eq!(
            item_fn.return_type().unwrap(),
            &syn::Type::Path(syn::TypePath {
                qself: None,
                path: syn::parse2::<syn::Path>(quote! { usize }).unwrap()
            })
        );
    }

    #[test]
    fn ref_returning_fn() {
        let input_tokens: TokenStream = quote! {
            pub fn get_magic_phrase() -> &'static str { "open sesame" }
        };

        let err = syn::parse2::<ExportedFn>(input_tokens).unwrap_err();
        assert_eq!(format!("{}", err), "cannot return a reference to Rhai");
    }

    #[test]
    fn ptr_returning_fn() {
        let input_tokens: TokenStream = quote! {
            pub fn get_magic_phrase() -> *const str { "open sesame" }
        };

        let err = syn::parse2::<ExportedFn>(input_tokens).unwrap_err();
        assert_eq!(format!("{}", err), "cannot return a pointer to Rhai");
    }

    #[test]
    fn ref_arg_fn() {
        let input_tokens: TokenStream = quote! {
            pub fn greet(who: &Person) { }
        };

        let err = syn::parse2::<ExportedFn>(input_tokens).unwrap_err();
        assert_eq!(
            format!("{}", err),
            "references from Rhai in this position must be mutable"
        );
    }

    #[test]
    fn ref_second_arg_fn() {
        let input_tokens: TokenStream = quote! {
            pub fn greet(count: usize, who: &Person) { }
        };

        let err = syn::parse2::<ExportedFn>(input_tokens).unwrap_err();
        assert_eq!(
            format!("{}", err),
            "this type in this position passes from Rhai by value"
        );
    }

    #[test]
    fn mut_ref_second_arg_fn() {
        let input_tokens: TokenStream = quote! {
            pub fn give(item_name: &str, who: &mut Person) { }
        };

        let err = syn::parse2::<ExportedFn>(input_tokens).unwrap_err();
        assert_eq!(
            format!("{}", err),
            "this type in this position passes from Rhai by value"
        );
    }

    #[test]
    fn str_arg_fn() {
        let input_tokens: TokenStream = quote! {
            pub fn log(message: &str) { }
        };

        let item_fn = syn::parse2::<ExportedFn>(input_tokens).unwrap();
        assert_eq!(&item_fn.name().to_string(), "log");
        assert_eq!(item_fn.arg_list().count(), 1);
        assert!(!item_fn.mutable_receiver());
        assert!(item_fn.is_public());
        assert!(item_fn.return_type().is_none());

        assert_eq!(
            item_fn.arg_list().next().unwrap(),
            &syn::parse2::<syn::FnArg>(quote! { message: &str }).unwrap()
        );
    }

    #[test]
    fn str_second_arg_fn() {
        let input_tokens: TokenStream = quote! {
            pub fn log(level: usize, message: &str) { }
        };

        let item_fn = syn::parse2::<ExportedFn>(input_tokens).unwrap();
        assert_eq!(&item_fn.name().to_string(), "log");
        assert_eq!(item_fn.arg_list().count(), 2);
        assert!(!item_fn.mutable_receiver());
        assert!(item_fn.is_public());
        assert!(item_fn.return_type().is_none());

        assert_eq!(
            item_fn.arg_list().next().unwrap(),
            &syn::parse2::<syn::FnArg>(quote! { level: usize }).unwrap()
        );
        assert_eq!(
            item_fn.arg_list().skip(1).next().unwrap(),
            &syn::parse2::<syn::FnArg>(quote! { message: &str }).unwrap()
        );
    }

    #[test]
    fn private_fn() {
        let input_tokens: TokenStream = quote! {
            fn do_nothing() { }
        };

        let item_fn = syn::parse2::<ExportedFn>(input_tokens).unwrap();
        assert_eq!(&item_fn.name().to_string(), "do_nothing");
        assert!(!item_fn.mutable_receiver());
        assert!(!item_fn.is_public());
        assert!(item_fn.return_type().is_none());
        assert_eq!(item_fn.arg_list().count(), 0);
    }

    #[test]
    fn receiver_fn() {
        let input_tokens: TokenStream = quote! {
            pub fn act_upon(&mut self) { }
        };

        let item_fn = syn::parse2::<ExportedFn>(input_tokens).unwrap();
        assert_eq!(&item_fn.name().to_string(), "act_upon");
        assert!(item_fn.mutable_receiver());
        assert!(item_fn.is_public());
        assert!(item_fn.return_type().is_none());
        assert_eq!(item_fn.arg_list().count(), 1);
    }

    #[test]
    fn immutable_receiver_fn() {
        let input_tokens: TokenStream = quote! {
            pub fn act_upon(&self) { }
        };

        let item_fn = syn::parse2::<ExportedFn>(input_tokens).unwrap();
        assert_eq!(&item_fn.name().to_string(), "act_upon");
        assert!(item_fn.mutable_receiver());
        assert!(item_fn.is_public());
        assert!(item_fn.return_type().is_none());
        assert_eq!(item_fn.arg_list().count(), 1);
    }
}

#[cfg(test)]
mod generate_tests {
    use super::ExportedFn;

    use proc_macro2::TokenStream;
    use quote::quote;

    fn assert_streams_eq(actual: TokenStream, expected: TokenStream) {
        let actual = actual.to_string();
        let expected = expected.to_string();
        if &actual != &expected {
            let mut counter = 0;
            let iter = actual
                .chars()
                .zip(expected.chars())
                .inspect(|_| counter += 1)
                .skip_while(|(a, e)| *a == *e);
            let (actual_diff, expected_diff) = {
                let mut actual_diff = String::new();
                let mut expected_diff = String::new();
                for (a, e) in iter.take(50) {
                    actual_diff.push(a);
                    expected_diff.push(e);
                }
                (actual_diff, expected_diff)
            };
            eprintln!("actual != expected, diverge at char {}", counter);
        }
        assert_eq!(actual, expected);
    }

    #[test]
    fn minimal_fn() {
        let input_tokens: TokenStream = quote! {
            pub fn do_nothing() { }
        };

        let expected_tokens = quote! {
            #[allow(unused)]
            pub mod rhai_fn_do_nothing {
                use super::*;
                struct Token();
                impl PluginFunction for Token {
                    fn call(&self,
                            args: &mut [&mut Dynamic], pos: Position
                    ) -> Result<Dynamic, Box<EvalAltResult>> {
                        if args.len() != 0usize {
                            return Err(Box::new(EvalAltResult::ErrorRuntime(
                                    format!("wrong arg count: {} != {}",
                                            args.len(), 0usize), Position::none())));
                        }
                        Ok(Dynamic::from(do_nothing()))
                    }

                    fn is_method_call(&self) -> bool { false }
                    fn is_varadic(&self) -> bool { false }
                    fn clone_boxed(&self) -> Box<dyn PluginFunction> { Box::new(Token()) }
                    fn input_types(&self) -> Box<[std::any::TypeId]> {
                        vec![].into_boxed_slice()
                    }
                }
                pub fn token_callable() -> CallableFunction {
                    CallableFunction::from_plugin(Token())
                }
                pub fn token_input_types() -> Box<[std::any::TypeId]> {
                    Token().input_types()
                }
            }
        };

        let item_fn = syn::parse2::<ExportedFn>(input_tokens).unwrap();
        assert_streams_eq(item_fn.generate(), expected_tokens);
    }

    #[test]
    fn one_arg_usize_fn() {
        let input_tokens: TokenStream = quote! {
            pub fn do_something(x: usize) { }
        };

        let expected_tokens = quote! {
            #[allow(unused)]
            pub mod rhai_fn_do_something {
                use super::*;
                struct Token();
                impl PluginFunction for Token {
                    fn call(&self,
                            args: &mut [&mut Dynamic], pos: Position
                    ) -> Result<Dynamic, Box<EvalAltResult>> {
                        if args.len() != 1usize {
                            return Err(Box::new(EvalAltResult::ErrorRuntime(
                                    format!("wrong arg count: {} != {}",
                                            args.len(), 1usize), Position::none())));
                        }
                        let arg0 = args[0usize].downcast_clone::<usize>().unwrap();
                        Ok(Dynamic::from(do_something(arg0)))
                    }

                    fn is_method_call(&self) -> bool { false }
                    fn is_varadic(&self) -> bool { false }
                    fn clone_boxed(&self) -> Box<dyn PluginFunction> { Box::new(Token()) }
                    fn input_types(&self) -> Box<[std::any::TypeId]> {
                        vec![std::any::TypeId::of::<usize>()].into_boxed_slice()
                    }
                }
                pub fn token_callable() -> CallableFunction {
                    CallableFunction::from_plugin(Token())
                }
                pub fn token_input_types() -> Box<[std::any::TypeId]> {
                    Token().input_types()
                }
            }
        };

        let item_fn = syn::parse2::<ExportedFn>(input_tokens).unwrap();
        assert_streams_eq(item_fn.generate(), expected_tokens);
    }

    #[test]
    fn one_arg_usize_fn_impl() {
        let input_tokens: TokenStream = quote! {
            pub fn do_something(x: usize) { }
        };

        let expected_tokens = quote! {
            impl PluginFunction for MyType {
                fn call(&self,
                        args: &mut [&mut Dynamic], pos: Position
                ) -> Result<Dynamic, Box<EvalAltResult>> {
                    if args.len() != 1usize {
                        return Err(Box::new(EvalAltResult::ErrorRuntime(
                                format!("wrong arg count: {} != {}",
                                        args.len(), 1usize), Position::none())));
                    }
                    let arg0 = args[0usize].downcast_clone::<usize>().unwrap();
                    Ok(Dynamic::from(do_something(arg0)))
                }

                fn is_method_call(&self) -> bool { false }
                fn is_varadic(&self) -> bool { false }
                fn clone_boxed(&self) -> Box<dyn PluginFunction> { Box::new(MyType()) }
                fn input_types(&self) -> Box<[std::any::TypeId]> {
                    vec![std::any::TypeId::of::<usize>()].into_boxed_slice()
                }
            }
        };

        let item_fn = syn::parse2::<ExportedFn>(input_tokens).unwrap();
        assert_streams_eq(item_fn.generate_impl("MyType"), expected_tokens);
    }

    #[test]
    fn two_arg_returning_usize_fn() {
        let input_tokens: TokenStream = quote! {
            pub fn add_together(x: usize, y: usize) -> usize { x + y }
        };

        let expected_tokens = quote! {
            #[allow(unused)]
            pub mod rhai_fn_add_together {
                use super::*;
                struct Token();
                impl PluginFunction for Token {
                    fn call(&self,
                            args: &mut [&mut Dynamic], pos: Position
                    ) -> Result<Dynamic, Box<EvalAltResult>> {
                        if args.len() != 2usize {
                            return Err(Box::new(EvalAltResult::ErrorRuntime(
                                    format!("wrong arg count: {} != {}",
                                            args.len(), 2usize), Position::none())));
                        }
                        let arg0 = args[0usize].downcast_clone::<usize>().unwrap();
                        let arg1 = args[1usize].downcast_clone::<usize>().unwrap();
                        Ok(Dynamic::from(add_together(arg0, arg1)))
                    }

                    fn is_method_call(&self) -> bool { false }
                    fn is_varadic(&self) -> bool { false }
                    fn clone_boxed(&self) -> Box<dyn PluginFunction> { Box::new(Token()) }
                    fn input_types(&self) -> Box<[std::any::TypeId]> {
                        vec![std::any::TypeId::of::<usize>(),
                             std::any::TypeId::of::<usize>()].into_boxed_slice()
                    }
                }
                pub fn token_callable() -> CallableFunction {
                    CallableFunction::from_plugin(Token())
                }
                pub fn token_input_types() -> Box<[std::any::TypeId]> {
                    Token().input_types()
                }
            }
        };

        let item_fn = syn::parse2::<ExportedFn>(input_tokens).unwrap();
        assert_streams_eq(item_fn.generate(), expected_tokens);
    }

    #[test]
    fn mut_arg_usize_fn() {
        let input_tokens: TokenStream = quote! {
            pub fn increment(x: &mut usize, y: usize) { *x += y; }
        };

        let expected_tokens = quote! {
            #[allow(unused)]
            pub mod rhai_fn_increment {
                use super::*;
                struct Token();
                impl PluginFunction for Token {
                    fn call(&self,
                            args: &mut [&mut Dynamic], pos: Position
                    ) -> Result<Dynamic, Box<EvalAltResult>> {
                        if args.len() != 2usize {
                            return Err(Box::new(EvalAltResult::ErrorRuntime(
                                    format!("wrong arg count: {} != {}",
                                            args.len(), 2usize), Position::none())));
                        }
                        let arg1 = args[1usize].downcast_clone::<usize>().unwrap();
                        let arg0: &mut _ = &mut args[0usize].write_lock::<usize>().unwrap();
                        Ok(Dynamic::from(increment(arg0, arg1)))
                    }

                    fn is_method_call(&self) -> bool { true }
                    fn is_varadic(&self) -> bool { false }
                    fn clone_boxed(&self) -> Box<dyn PluginFunction> { Box::new(Token()) }
                    fn input_types(&self) -> Box<[std::any::TypeId]> {
                        vec![std::any::TypeId::of::<usize>(),
                             std::any::TypeId::of::<usize>()].into_boxed_slice()
                    }
                }
                pub fn token_callable() -> CallableFunction {
                    CallableFunction::from_plugin(Token())
                }
                pub fn token_input_types() -> Box<[std::any::TypeId]> {
                    Token().input_types()
                }
            }
        };

        let item_fn = syn::parse2::<ExportedFn>(input_tokens).unwrap();
        assert!(item_fn.mutable_receiver());
        assert_streams_eq(item_fn.generate(), expected_tokens);
    }

    #[test]
    fn str_arg_fn() {
        let input_tokens: TokenStream = quote! {
            pub fn special_print(message: &str) { eprintln!("----{}----", message); }
        };

        let expected_tokens = quote! {
            #[allow(unused)]
            pub mod rhai_fn_special_print {
                use super::*;
                struct Token();
                impl PluginFunction for Token {
                    fn call(&self,
                            args: &mut [&mut Dynamic], pos: Position
                    ) -> Result<Dynamic, Box<EvalAltResult>> {
                        if args.len() != 1usize {
                            return Err(Box::new(EvalAltResult::ErrorRuntime(
                                    format!("wrong arg count: {} != {}",
                                            args.len(), 1usize), Position::none())));
                        }
                        let arg0 = args[0usize].downcast_clone::<ImmutableString>().unwrap();
                        Ok(Dynamic::from(special_print(&arg0)))
                    }

                    fn is_method_call(&self) -> bool { false }
                    fn is_varadic(&self) -> bool { false }
                    fn clone_boxed(&self) -> Box<dyn PluginFunction> { Box::new(Token()) }
                    fn input_types(&self) -> Box<[std::any::TypeId]> {
                        vec![std::any::TypeId::of::<ImmutableString>()].into_boxed_slice()
                    }
                }
                pub fn token_callable() -> CallableFunction {
                    CallableFunction::from_plugin(Token())
                }
                pub fn token_input_types() -> Box<[std::any::TypeId]> {
                    Token().input_types()
                }
            }
        };

        let item_fn = syn::parse2::<ExportedFn>(input_tokens).unwrap();
        assert!(!item_fn.mutable_receiver());
        assert_streams_eq(item_fn.generate(), expected_tokens);
    }
}

#[cfg(test)]
mod ui_tests {
    #[test]
    fn all() {
        let t = trybuild::TestCases::new();
        t.compile_fail("ui_tests/*.rs");
    }
}
