/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use proc_macro2::{Ident, TokenStream};
use quote::{format_ident, quote};
use syn::parse::ParseStream;
use syn::{Attribute, Data, DeriveInput, Fields, LitInt, Token, Type, parse_macro_input};

#[proc_macro_derive(Proto, attributes(proto))]
pub fn derive_proto(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    expand(&input)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

#[derive(Default)]
struct Versions {
    since: u16,
    default: bool,
    oldest: Option<u16>,
}

fn versions(attrs: &[Attribute]) -> syn::Result<Versions> {
    let mut out = Versions::default();
    for attr in attrs.iter().filter(|a| a.path().is_ident("proto")) {
        attr.parse_args_with(|input: ParseStream| parse_versions(input, &mut out))?;
        if out.default && out.since == 0 {
            return Err(syn::Error::new_spanned(
                attr,
                "`default` needs the version that added the field",
            ));
        }
    }

    Ok(out)
}

fn parse_versions(input: ParseStream, out: &mut Versions) -> syn::Result<()> {
    while !input.is_empty() {
        if input.peek(LitInt) {
            out.since = input.parse::<LitInt>()?.base10_parse()?;
        } else {
            let word: Ident = input.parse()?;
            match word.to_string().as_str() {
                "default" => out.default = true,
                "oldest" => {
                    input.parse::<Token![=]>()?;
                    out.oldest = Some(input.parse::<LitInt>()?.base10_parse()?);
                }
                _ => {
                    return Err(syn::Error::new(
                        word.span(),
                        "expected a version, `default` or `oldest = N`",
                    ));
                }
            }
        }

        if !input.is_empty() {
            input.parse::<Token![,]>()?;
        }
    }

    Ok(())
}

struct Field {
    member: Option<Ident>,
    binding: Ident,
    ty: Type,
    since: u16,
    default: bool,
}

fn fields(fields: &Fields) -> syn::Result<Vec<Field>> {
    fields
        .iter()
        .enumerate()
        .map(|(i, f)| {
            let v = versions(&f.attrs)?;
            if v.oldest.is_some() {
                return Err(syn::Error::new_spanned(f, "`oldest` belongs on the type"));
            }

            Ok(Field {
                member: f.ident.clone(),
                binding: format_ident!("__f{i}"),
                ty: f.ty.clone(),
                since: v.since,
                default: v.default,
            })
        })
        .collect()
}

fn codec() -> TokenStream {
    quote!(::gradient_wire::codec)
}

fn pattern(path: TokenStream, fields: &[Field], shape: &Fields) -> TokenStream {
    let bindings = fields.iter().map(|f| &f.binding);
    match shape {
        Fields::Named(_) => {
            let members = fields.iter().map(|f| &f.member);
            quote!(#path { #(#members: #bindings),* })
        }
        Fields::Unnamed(_) => quote!(#path ( #(#bindings),* )),
        Fields::Unit => path,
    }
}

fn when_present(since: u16, body: TokenStream) -> TokenStream {
    if since > 0 {
        quote!(if version >= #since { #body })
    } else {
        quote!({ #body })
    }
}

fn field_versions(fields: &[Field], present_since: u16) -> (Vec<TokenStream>, Vec<TokenStream>) {
    let codec = codec();
    fields
        .iter()
        .map(|f| {
            let ty = &f.ty;
            let since = f.since.max(present_since);
            let oldest = if f.default {
                quote!(#codec::floor(<#ty as #codec::Proto>::OLDEST, #since))
            } else {
                let needed = f.since;
                quote!(#codec::floor(#codec::max(#needed, <#ty as #codec::Proto>::OLDEST), #present_since))
            };
            let newest = quote!(#codec::max(#since, <#ty as #codec::Proto>::NEWEST));
            (oldest, newest)
        })
        .unzip()
}

fn encode_fields(fields: &[Field], present_since: u16) -> TokenStream {
    let codec = codec();
    let steps = fields.iter().map(|f| {
        let binding = &f.binding;
        let encode = quote!(#codec::Proto::encode(#binding, version, out)?;);
        if f.default {
            when_present(f.since.max(present_since), encode)
        } else {
            encode
        }
    });
    quote!(#(#steps)*)
}

fn decode_fields(fields: &[Field], present_since: u16) -> TokenStream {
    let codec = codec();
    let steps = fields.iter().map(|f| {
        let (binding, ty) = (&f.binding, &f.ty);
        let since = f.since.max(present_since);
        let decode = quote!(<#ty as #codec::Proto>::decode(input, version)?);
        if f.default {
            quote!(let #binding = if version >= #since { #decode } else { ::core::default::Default::default() };)
        } else {
            quote!(let #binding = #decode;)
        }
    });
    quote!(#(#steps)*)
}

fn describe_fields(fields: &[Field], present_since: u16) -> TokenStream {
    if fields.is_empty() {
        return quote!(out.push_str("{}"););
    }

    let codec = codec();
    let steps = fields.iter().map(|f| {
        let ty = &f.ty;
        when_present(
            f.since.max(present_since),
            quote! {
                #codec::push_separator(out, &mut first);
                <#ty as #codec::Proto>::describe(version, out);
            },
        )
    });
    quote!({
        out.push('{');
        let mut first = true;
        #(#steps)*
        out.push('}');
    })
}

struct Body {
    oldest: Vec<TokenStream>,
    newest: Vec<TokenStream>,
    encode: TokenStream,
    decode: TokenStream,
    describe: TokenStream,
}

fn struct_body(shape: &Fields) -> syn::Result<Body> {
    let fs = fields(shape)?;
    let (oldest, newest) = field_versions(&fs, 0);
    let pat = pattern(quote!(Self), &fs, shape);
    let encode = encode_fields(&fs, 0);
    let decode = decode_fields(&fs, 0);
    let bind = (!fs.is_empty()).then(|| quote!(let #pat = self;));
    Ok(Body {
        oldest,
        newest,
        encode: quote! { #bind #encode Ok(()) },
        decode: quote! { #decode Ok(#pat) },
        describe: describe_fields(&fs, 0),
    })
}

fn enum_body(name: &Ident, data: &syn::DataEnum) -> syn::Result<Body> {
    let codec = codec();
    let mut body = Body {
        oldest: Vec::new(),
        newest: Vec::new(),
        encode: TokenStream::new(),
        decode: TokenStream::new(),
        describe: TokenStream::new(),
    };
    let mut encode_arms = Vec::new();
    let mut decode_arms = Vec::new();
    let mut describe_arms = Vec::new();
    let mut previous = 0u16;
    for (index, variant) in data.variants.iter().enumerate() {
        let v = versions(&variant.attrs)?;
        if v.default || v.oldest.is_some() {
            return Err(syn::Error::new_spanned(
                variant,
                "a variant takes only its version",
            ));
        }

        if v.since < previous {
            return Err(syn::Error::new_spanned(
                variant,
                "variants are append-only: a variant may not be older than the one before it",
            ));
        }

        previous = v.since;
        let since = v.since;
        let tag = index as u64;
        let ident = &variant.ident;
        let variant_label = ident.to_string();
        let shape_label = format!("{index}:");
        let fs = fields(&variant.fields)?;
        let (oldest, newest) = field_versions(&fs, since);
        body.oldest.extend(oldest);
        body.newest.extend(newest);
        body.newest.push(quote!(#since));
        let pat = pattern(quote!(Self::#ident), &fs, &variant.fields);
        let encode = encode_fields(&fs, since);
        let decode = decode_fields(&fs, since);
        let describe = describe_fields(&fs, since);
        let refuse_older_peer = (since > 0).then(|| {
            quote! {
                if version < #since {
                    return Err(#codec::EncodeError::NewerThanPeer { variant: #variant_label, since: #since, version });
                }
            }
        });
        let guard = (since > 0).then(|| quote!(if version >= #since));
        encode_arms.push(quote! {
            #pat => { #refuse_older_peer #codec::put_varint(out, #tag); #encode }
        });
        decode_arms.push(quote! {
            #tag #guard => { #decode Ok(#pat) }
        });
        describe_arms.push(when_present(
            since,
            quote! {
                #codec::push_separator(out, &mut first);
                out.push_str(#shape_label);
                #describe
            },
        ));
    }

    let ty = name.to_string();
    body.encode = quote! { match self { #(#encode_arms)* } Ok(()) };
    body.decode = quote! {
        match #codec::get_varint(input)? {
            #(#decode_arms)*
            tag => Err(#codec::DecodeError::UnknownVariant { ty: #ty, tag, version }),
        }
    };
    body.describe = quote! {
        out.push('[');
        let mut first = true;
        #(#describe_arms)*
        out.push(']');
    };
    Ok(body)
}

fn expand(input: &DeriveInput) -> syn::Result<TokenStream> {
    let name = &input.ident;
    let top = versions(&input.attrs)?;
    if top.since != 0 || top.default {
        return Err(syn::Error::new_spanned(
            name,
            "only `oldest = N` belongs on the type",
        ));
    }

    let oldest_attr = top.oldest.unwrap_or(0);
    let codec = codec();
    let Body {
        oldest,
        newest,
        encode,
        decode,
        describe,
    } = match &input.data {
        Data::Struct(data) => struct_body(&data.fields)?,
        Data::Enum(data) => enum_body(name, data)?,
        Data::Union(_) => {
            return Err(syn::Error::new_spanned(name, "unions are not supported"));
        }
    };

    let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();
    Ok(quote! {
        impl #impl_generics #codec::Proto for #name #ty_generics #where_clause {
            const OLDEST: u16 = #codec::max_of(&[#oldest_attr #(, #oldest)*]);
            const NEWEST: u16 = #codec::max_of(&[#oldest_attr #(, #newest)*]);

            #[allow(unused_variables, reason = "types without fields read nothing")]
            fn encode(&self, version: u16, out: &mut #codec::bytes::BytesMut) -> ::core::result::Result<(), #codec::EncodeError> {
                #encode
            }

            #[allow(unused_variables, reason = "types without fields read nothing")]
            fn decode(input: &mut #codec::bytes::Bytes, version: u16) -> ::core::result::Result<Self, #codec::DecodeError> {
                #decode
            }

            #[allow(unused_variables, reason = "types without fields read nothing")]
            fn describe(version: u16, out: &mut ::std::string::String) {
                #describe
            }
        }
    })
}
