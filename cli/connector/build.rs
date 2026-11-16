// SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
// SPDX-License-Identifier: AGPL-3.0-only

use serde_json::{Map, Value, json};
use std::path::{Path, PathBuf};
use std::{env, fs};

const SPEC: &str = "../../docs/gradient-api.yaml";
const SCHEMA_REF_PREFIX: &str = "#/components/schemas/";
const DEFINITION_REF_PREFIX: &str = "#/definitions/";
const TEXT_FORMATS: [&str; 2] = ["date", "date-time"];

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let spec_path = manifest_dir.join(SPEC);
    println!("cargo:rerun-if-changed={}", spec_path.display());
    println!("cargo:rerun-if-changed=build.rs");

    let mut schemas = read_schemas(&spec_path);
    merge_all_of_object_schemas(&mut schemas);
    inline_unconstrained(&mut schemas);
    visit_objects(&mut schemas, &mut |map| {
        rewrite_ref(map);
        drop_temporal_format(map);
    });

    let code = generate(schemas);
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    fs::write(out_dir.join("api.rs"), code).expect("write api.rs");
}

fn read_schemas(spec_path: &Path) -> Value {
    let text = fs::read_to_string(spec_path).expect("read gradient-api.yaml");
    let spec: Value = yaml_serde::from_str(&text).expect("parse gradient-api.yaml");
    spec["components"]["schemas"].clone()
}

fn visit_objects(node: &mut Value, visitor: &mut impl FnMut(&mut Map<String, Value>)) {
    match node {
        Value::Object(map) => {
            visitor(map);
            map.values_mut()
                .for_each(|child| visit_objects(child, visitor));
        }
        Value::Array(items) => items
            .iter_mut()
            .for_each(|child| visit_objects(child, visitor)),
        _ => {}
    }
}

fn merge_all_of_object_schemas(schemas: &mut Value) {
    let definitions = schemas.clone();
    visit_objects(schemas, &mut |map| {
        let Some(Value::Array(parts)) = map.get("allOf") else {
            return;
        };
        let Some(objects) = parts
            .iter()
            .map(|part| resolve_all_of_part_object_schema(part, &definitions))
            .collect::<Option<Vec<_>>>()
        else {
            return;
        };
        let mut properties = Map::new();
        let mut required = Vec::new();
        for object in objects {
            if let Some(Value::Object(object_properties)) = object.get("properties") {
                properties.extend(object_properties.clone());
            }
            if let Some(Value::Array(names)) = object.get("required") {
                required.extend(names.iter().cloned());
            }
        }
        map.remove("allOf");
        map.insert("type".into(), json!("object"));
        map.insert("properties".into(), Value::Object(properties));
        if !required.is_empty() {
            map.insert("required".into(), Value::Array(required));
        }
    });
}

fn resolve_all_of_part_object_schema<'a>(
    part: &'a Value,
    definitions: &'a Value,
) -> Option<&'a Map<String, Value>> {
    let schema = match part.get("$ref").and_then(Value::as_str) {
        Some(target) => definitions.get(target.strip_prefix(SCHEMA_REF_PREFIX)?)?,
        None => part,
    };
    let object = schema.as_object()?;
    (object.get("type").and_then(Value::as_str) == Some("object")).then_some(object)
}

fn inline_unconstrained(schemas: &mut Value) {
    let Some(definitions) = schemas.as_object_mut() else {
        return;
    };
    let unconstrained: Vec<String> = definitions
        .iter()
        .filter(|(_, schema)| schema.as_object().is_some_and(Map::is_empty))
        .map(|(name, _)| name.clone())
        .collect();
    definitions.retain(|name, _| !unconstrained.contains(name));
    visit_objects(schemas, &mut |map| {
        let targets_unconstrained = map
            .get("$ref")
            .and_then(Value::as_str)
            .and_then(|target| target.strip_prefix(SCHEMA_REF_PREFIX))
            .is_some_and(|name| unconstrained.iter().any(|candidate| candidate == name));
        if targets_unconstrained {
            map.clear();
        }
    });
}

fn rewrite_ref(map: &mut Map<String, Value>) {
    if let Some(Value::String(target)) = map.get_mut("$ref")
        && let Some(name) = target.strip_prefix(SCHEMA_REF_PREFIX)
    {
        *target = format!("{DEFINITION_REF_PREFIX}{name}");
    }
}

fn drop_temporal_format(map: &mut Map<String, Value>) {
    if map
        .get("format")
        .and_then(Value::as_str)
        .is_some_and(|format| TEXT_FORMATS.contains(&format))
    {
        map.remove("format");
    }
}

fn generate(definitions: Value) -> String {
    let root: schemars::schema::RootSchema =
        serde_json::from_value(json!({ "definitions": definitions })).expect("root schema");

    let mut settings = typify::TypeSpaceSettings::default();
    settings.with_derive("PartialEq".to_string());
    let mut type_space = typify::TypeSpace::new(&settings);
    type_space.add_root_schema(root).expect("add schemas");

    let file = syn::parse2(type_space.to_stream()).expect("parse generated code");
    prettyplease::unparse(&file)
}
