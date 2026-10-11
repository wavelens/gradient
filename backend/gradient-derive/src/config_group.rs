/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use proc_macro2::{TokenStream, TokenTree};
use quote::quote;
use syn::parse::{Parse, ParseStream};
use syn::{Attribute, Data, DeriveInput, Expr, Field, Fields, Ident, Lit, LitStr, Meta, Token};

struct Group {
    name: String,
    flag_prefix: String,
    env_prefix: String,
    default: bool,
}

impl Parse for Group {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let mut name = String::new();
        let mut env_only = None;
        let mut default = true;
        while !input.is_empty() {
            if input.peek(LitStr) {
                name = input.parse::<LitStr>()?.value();
            } else {
                let key: Ident = input.parse()?;
                match key.to_string().as_str() {
                    "env" => {
                        input.parse::<Token![=]>()?;
                        env_only = Some(input.parse::<LitStr>()?.value());
                    }
                    "no_default" => default = false,
                    _ => {
                        return Err(syn::Error::new_spanned(
                            key,
                            "expected `env` or `no_default`",
                        ));
                    }
                }
            }

            if !input.is_empty() {
                input.parse::<Token![,]>()?;
            }
        }

        let prefix = |group: &str, separator: char| match group {
            "" => String::new(),
            group => format!(
                "{}{separator}",
                group.replace(['_', '-'], &separator.to_string())
            ),
        };

        Ok(Self {
            flag_prefix: prefix(&name, '-'),
            env_prefix: format!(
                "GRADIENT_{}",
                prefix(env_only.as_deref().unwrap_or(&name), '_').to_uppercase()
            ),
            name,
            default,
        })
    }
}

pub fn expand(args: TokenStream, input: TokenStream) -> syn::Result<TokenStream> {
    let group: Group = syn::parse2(args)?;
    let mut input: DeriveInput = syn::parse2(input)?;
    let Data::Struct(data) = &mut input.data else {
        return Err(syn::Error::new_spanned(
            &input.ident,
            "`config_group` needs a struct",
        ));
    };
    let Fields::Named(fields) = &mut data.fields else {
        return Err(syn::Error::new_spanned(
            &input.ident,
            "`config_group` needs named fields",
        ));
    };

    let mut own = Vec::new();
    let mut nested = Vec::new();
    for field in &mut fields.named {
        if field
            .attrs
            .iter()
            .any(|attr| attr.path().is_ident("command"))
        {
            let ty = &field.ty;
            nested.push(quote!(<#ty>::options()));
        } else {
            own.push(name_option(field, &group)?);
        }
    }

    let name = &input.ident;
    let default = group.default.then(|| {
        quote! {
            impl ::core::default::Default for #name {
                fn default() -> Self {
                    let command = <Self as ::clap::Args>::augment_args(::clap::Command::new("defaults"))
                        .mut_args(|arg| arg.env(None));

                    <Self as ::clap::FromArgMatches>::from_arg_matches(&command.get_matches_from(["defaults"]))
                        .expect("every option of the group has a default")
                }
            }
        }
    });

    Ok(quote! {
        #input

        #default

        impl #name {
            pub fn options() -> ::std::vec::Vec<::gradient_types::cli::OptionSpec> {
                [::std::vec![#(#own),*] #(, #nested)*].concat()
            }
        }
    })
}

fn name_option(field: &mut Field, group: &Group) -> syn::Result<TokenStream> {
    let ident = field.ident.clone().expect("named field");
    for attr in field
        .attrs
        .iter()
        .filter(|attr| attr.path().is_ident("arg"))
    {
        reject_hand_written_names(attr)?;
    }

    let flag_only = field
        .attrs
        .iter()
        .any(|attr| attr.path().is_ident("flag_only"));
    field
        .attrs
        .retain(|attr| !attr.path().is_ident("flag_only"));

    let flag = format!(
        "{}{}",
        group.flag_prefix,
        ident.to_string().replace('_', "-")
    );
    let env_name = format!("{}{}", group.env_prefix, ident.to_string().to_uppercase());
    let (env_arg, env) = if flag_only {
        (quote!(), quote!(::core::option::Option::None))
    } else {
        (
            quote!(, env = #env_name),
            quote!(::core::option::Option::Some(#env_name)),
        )
    };
    field
        .attrs
        .push(syn::parse_quote!(#[arg(long = #flag #env_arg)]));

    let group_name = &group.name;
    let field_name = ident.to_string();
    let ty = &field.ty;
    let rust_type = quote!(#ty).to_string().replace(' ', "");
    let description = description(&field.attrs);

    Ok(quote! {
        ::gradient_types::cli::OptionSpec {
            group: #group_name,
            field: #field_name,
            flag: #flag,
            env: #env,
            rust_type: #rust_type,
            description: #description,
        }
    })
}

fn reject_hand_written_names(attr: &Attribute) -> syn::Result<()> {
    let Meta::List(list) = &attr.meta else {
        return Ok(());
    };

    let mut starts_entry = true;
    for token in list.tokens.clone() {
        match &token {
            TokenTree::Ident(ident) if starts_entry && (ident == "long" || ident == "env") => {
                return Err(syn::Error::new_spanned(
                    ident,
                    "`config_group` derives the flag and the env name from the group and the field",
                ));
            }
            TokenTree::Punct(punct) => starts_entry = punct.as_char() == ',',
            _ => starts_entry = false,
        }
    }

    Ok(())
}

fn description(attrs: &[Attribute]) -> String {
    attrs
        .iter()
        .filter(|attr| attr.path().is_ident("doc"))
        .filter_map(|attr| match &attr.meta {
            Meta::NameValue(doc) => match &doc.value {
                Expr::Lit(literal) => match &literal.lit {
                    Lit::Str(text) => Some(text.value()),
                    _ => None,
                },
                _ => None,
            },
            _ => None,
        })
        .map(|line| line.strip_prefix(' ').unwrap_or(&line).to_owned())
        .collect::<Vec<_>>()
        .join("\n")
}
