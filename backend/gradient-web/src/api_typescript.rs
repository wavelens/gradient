/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::fmt::Write;
use utoipa::openapi::schema::{
    AdditionalProperties, Array, ArrayItems, Object, Schema, SchemaType, Type,
};
use utoipa::openapi::{OpenApi as Spec, RefOr};

const HEADER: &str =
    "// Generated from docs/gradient-api.yaml by gradient-api-spec. Do not edit.\n";
const INDENT: &str = "  ";

pub fn typescript(spec: &Spec) -> String {
    let mut out = String::from(HEADER);
    let schemas = spec.components.iter().flat_map(|c| c.schemas.iter());
    for (name, schema) in schemas {
        out.push('\n');
        match schema {
            RefOr::T(Schema::Object(object)) if !object.properties.is_empty() => {
                let _ = writeln!(
                    out,
                    "export interface {} {}",
                    type_name(name),
                    fields(object, 0)
                );
            }
            other => {
                let _ = writeln!(
                    out,
                    "export type {} = {};",
                    type_name(name),
                    expression(other, 0)
                );
            }
        }
    }
    out
}

fn type_name(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

fn expression(schema: &RefOr<Schema>, depth: usize) -> String {
    match schema {
        RefOr::Ref(reference) => type_name(
            reference
                .ref_location
                .rsplit('/')
                .next()
                .unwrap_or(&reference.ref_location),
        ),
        RefOr::T(Schema::Object(object)) => object_expression(object, depth),
        RefOr::T(Schema::Array(array)) => array_expression(array, depth),
        RefOr::T(Schema::OneOf(one_of)) => joined(&one_of.items, " | ", depth),
        RefOr::T(Schema::AnyOf(any_of)) => joined(&any_of.items, " | ", depth),
        RefOr::T(Schema::AllOf(all_of)) => joined(&all_of.items, " & ", depth),
        RefOr::T(_) => "unknown".to_string(),
    }
}

fn joined(items: &[RefOr<Schema>], separator: &str, depth: usize) -> String {
    let parts: Vec<String> = items.iter().map(|item| expression(item, depth)).collect();
    match parts.as_slice() {
        [] => "unknown".to_string(),
        [single] => single.clone(),
        _ => parts.join(separator),
    }
}

fn object_expression(object: &Object, depth: usize) -> String {
    if let Some(values) = &object.enum_values {
        let literals: Vec<String> = values.iter().map(|value| value.to_string()).collect();
        return with_null(literals.join(" | "), &object.schema_type);
    }
    match &object.schema_type {
        SchemaType::AnyValue => "unknown".to_string(),
        SchemaType::Type(kind) => kind_expression(kind, object, depth),
        SchemaType::Array(kinds) => kinds
            .iter()
            .map(|kind| kind_expression(kind, object, depth))
            .collect::<Vec<_>>()
            .join(" | "),
    }
}

fn kind_expression(kind: &Type, object: &Object, depth: usize) -> String {
    match kind {
        Type::String => "string".to_string(),
        Type::Integer | Type::Number => "number".to_string(),
        Type::Boolean => "boolean".to_string(),
        Type::Null => "null".to_string(),
        Type::Array => "unknown[]".to_string(),
        Type::Object if !object.properties.is_empty() => fields(object, depth),
        Type::Object => match object.additional_properties.as_deref() {
            Some(AdditionalProperties::RefOr(value)) => {
                format!("{{ [key: string]: {} }}", expression(value, depth))
            }
            _ => "{ [key: string]: unknown }".to_string(),
        },
    }
}

fn array_expression(array: &Array, depth: usize) -> String {
    let item = match &array.items {
        ArrayItems::RefOrSchema(item) => expression(item, depth),
        ArrayItems::False => "never".to_string(),
    };
    let list = if item.contains(' ') {
        format!("({item})[]")
    } else {
        format!("{item}[]")
    };
    with_null(list, &array.schema_type)
}

fn with_null(expression: String, schema_type: &SchemaType) -> String {
    match schema_type {
        SchemaType::Array(kinds) if kinds.contains(&Type::Null) => format!("{expression} | null"),
        _ => expression,
    }
}

fn fields(object: &Object, depth: usize) -> String {
    let pad = INDENT.repeat(depth + 1);
    let mut out = String::from("{\n");
    for (name, schema) in &object.properties {
        let optional = if object.required.contains(name) {
            ""
        } else {
            "?"
        };
        let _ = writeln!(
            out,
            "{pad}{}{optional}: {};",
            property_name(name),
            expression(schema, depth + 1)
        );
    }
    out.push_str(&INDENT.repeat(depth));
    out.push('}');
    out
}

fn property_name(name: &str) -> String {
    let plain = name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
        && !name.starts_with(|c: char| c.is_ascii_digit());
    if plain {
        name.to_string()
    } else {
        format!("'{}'", name.replace('\'', "\\'"))
    }
}
