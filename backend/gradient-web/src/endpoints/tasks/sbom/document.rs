/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use gradient_db::graph::runtime_closure::parse_reference_hash;
use gradient_db::graph::sbom::{DerivationName, EntryPointOutput};
use gradient_entity::entry_point::License;
use gradient_types::ids::EvaluationId;
use gradient_types::{MCachedPath, StorePathMapper};
use gradient_util::nix_hash::{nix32_decode, normalize_nar_hash, strip_hash_algo};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::store_paths::original_text;

const TASK_REF: &str = "task";

pub struct SbomInput {
    pub task: String,
    pub evaluation: EvaluationId,
    pub commit: Option<String>,
    pub created_at: NaiveDateTime,
    pub entry_points: Vec<EntryPointOutput>,
    pub reached: HashMap<String, MCachedPath>,
    pub names: HashMap<String, DerivationName>,
}

struct Component<'a> {
    hash: String,
    package: String,
    cached: Option<&'a MCachedPath>,
    attributes: BTreeSet<String>,
    licenses: Vec<License>,
}

impl Component<'_> {
    fn bom_ref(&self) -> String {
        format!("{}-{}", self.hash, self.package)
    }
}

pub fn cyclonedx(input: &SbomInput) -> Value {
    let components = components(input);
    let uncached: Vec<String> = components
        .values()
        .filter(|c| c.cached.is_none())
        .map(Component::bom_ref)
        .collect();

    json!({
        "bomFormat": "CycloneDX",
        "specVersion": "1.6",
        "serialNumber": format!("urn:uuid:{}", input.evaluation),
        "version": 1,
        "metadata": metadata(input, uncached.is_empty()),
        "components": components
            .values()
            .map(|c| component_json(c, input.names.get(&c.hash)))
            .collect::<Vec<_>>(),
        "dependencies": dependencies(input, &components),
        "compositions": [composition(uncached)],
    })
}

pub fn component_hashes(
    entry_points: &[EntryPointOutput],
    reached: &HashMap<String, MCachedPath>,
) -> Vec<String> {
    let mut hashes: BTreeSet<String> = reached.keys().cloned().collect();
    hashes.extend(entry_points.iter().map(|ep| ep.hash.clone()));
    hashes.extend(
        reached
            .values()
            .flat_map(references)
            .map(|(hash, _)| hash),
    );
    hashes.into_iter().collect()
}

pub fn with_original_hashes(mut input: SbomInput, mapper: &StorePathMapper) -> SbomInput {
    let original = |text: String| original_text(mapper, text);
    for ep in &mut input.entry_points {
        ep.hash = original(std::mem::take(&mut ep.hash));
    }
    input.reached = input
        .reached
        .into_values()
        .map(|mut path| {
            path.hash = original(path.hash);
            path.references = path.references.map(|refs| {
                refs.split_whitespace()
                    .map(|token| original(token.to_owned()))
                    .collect::<Vec<_>>()
                    .join(" ")
            });
            (path.hash.clone(), path)
        })
        .collect();
    input.names = input
        .names
        .into_iter()
        .map(|(hash, name)| (original(hash), name))
        .collect();
    input
}

fn components(input: &SbomInput) -> BTreeMap<String, Component<'_>> {
    let mut components: BTreeMap<String, Component<'_>> = BTreeMap::new();
    for path in input.reached.values() {
        add(&mut components, &path.hash, &path.package, Some(path));
    }
    for (hash, package) in input.reached.values().flat_map(references) {
        add(&mut components, &hash, &package, None);
    }
    for ep in &input.entry_points {
        add(&mut components, &ep.hash, &ep.package, None);
    }

    for ep in &input.entry_points {
        let component = components.get_mut(&ep.hash).expect("added above");
        component.attributes.insert(ep.attr.clone());
        for license in &ep.licenses.0 {
            if !component.licenses.contains(license) {
                component.licenses.push(license.clone());
            }
        }
    }
    components
}

fn add<'a>(
    components: &mut BTreeMap<String, Component<'a>>,
    hash: &str,
    package: &str,
    cached: Option<&'a MCachedPath>,
) {
    components
        .entry(hash.to_owned())
        .or_insert_with(|| Component {
            hash: hash.to_owned(),
            package: package.to_owned(),
            cached,
            attributes: BTreeSet::new(),
            licenses: Vec::new(),
        });
}

fn references(path: &MCachedPath) -> Vec<(String, String)> {
    path.references
        .as_deref()
        .unwrap_or_default()
        .split_whitespace()
        .filter_map(|token| {
            let hash = parse_reference_hash(token)?;
            let base = token.rsplit('/').next().unwrap_or(token);
            let package = base.strip_prefix(&format!("{hash}-"))?.to_owned();
            (hash != path.hash).then_some((hash, package))
        })
        .collect()
}

fn metadata(input: &SbomInput, complete: bool) -> Value {
    let mut task = json!({
        "type": "application",
        "bom-ref": TASK_REF,
        "name": input.task,
    });
    if let Some(commit) = &input.commit {
        task["version"] = json!(commit);
    }

    json!({
        "timestamp": input.created_at.and_utc().to_rfc3339(),
        "tools": { "components": [{
            "type": "application",
            "name": "gradient",
            "version": env!("CARGO_PKG_VERSION"),
        }]},
        "component": task,
        "properties": [{ "name": "gradient:complete", "value": complete.to_string() }],
    })
}

fn component_json(component: &Component<'_>, name: Option<&DerivationName>) -> Value {
    let pname = name.and_then(|n| n.pname.clone());
    let version = name.zip(pname.as_deref()).and_then(|(n, pname)| {
        n.name
            .strip_prefix(pname)
            .and_then(|rest| rest.strip_prefix('-'))
            .filter(|rest| !rest.is_empty())
            .map(str::to_owned)
    });

    let mut value = json!({
        "type": "library",
        "bom-ref": component.bom_ref(),
        "name": pname.clone().unwrap_or_else(|| component.package.clone()),
        "properties": properties(component),
    });
    if let Some(version) = &version {
        value["version"] = json!(version);
    }
    if let Some(pname) = &pname {
        value["purl"] = json!(match &version {
            Some(version) => format!("pkg:nix/{pname}@{version}"),
            None => format!("pkg:nix/{pname}"),
        });
    }
    if let Some(hex) = component.cached.and_then(nar_hash_hex) {
        value["hashes"] = json!([{ "alg": "SHA-256", "content": hex }]);
    }
    if !component.licenses.is_empty() {
        value["licenses"] = component.licenses.iter().map(license_json).collect();
    }
    value
}

fn properties(component: &Component<'_>) -> Vec<Value> {
    let property = |name: &str, value: String| json!({ "name": name, "value": value });
    let mut properties = vec![property(
        "nix:store_path",
        format!("/nix/store/{}", component.bom_ref()),
    )];
    if let Some(size) = component.cached.and_then(|c| c.nar_size) {
        properties.push(property("nix:nar_size", size.to_string()));
    }
    properties.extend(
        component
            .attributes
            .iter()
            .map(|attr| property("nix:attribute", attr.clone())),
    );
    properties
}

fn nar_hash_hex(path: &MCachedPath) -> Option<String> {
    let normalized = normalize_nar_hash(path.nar_hash.as_deref()?);
    if !normalized.starts_with("sha256:") {
        return None;
    }
    let bytes = nix32_decode(strip_hash_algo(&normalized))?;
    Some(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

fn license_json(license: &License) -> Value {
    match license {
        License::Spdx { id } => json!({ "license": { "id": id } }),
        License::Name { name } => json!({ "license": { "name": name } }),
    }
}

fn dependencies(input: &SbomInput, components: &BTreeMap<String, Component<'_>>) -> Vec<Value> {
    let bom_ref = |hash: &str| components.get(hash).map(Component::bom_ref);
    let entry_points: BTreeSet<String> = input
        .entry_points
        .iter()
        .filter_map(|ep| bom_ref(&ep.hash))
        .collect();

    let mut dependencies = vec![json!({ "ref": TASK_REF, "dependsOn": entry_points })];
    dependencies.extend(components.values().map(|component| {
        let depends_on: BTreeSet<String> = component
            .cached
            .map(references)
            .unwrap_or_default()
            .iter()
            .filter_map(|(hash, _)| bom_ref(hash))
            .collect();
        json!({ "ref": component.bom_ref(), "dependsOn": depends_on })
    }));
    dependencies
}

fn composition(uncached: Vec<String>) -> Value {
    if uncached.is_empty() {
        json!({ "aggregate": "complete", "assemblies": [TASK_REF] })
    } else {
        json!({ "aggregate": "incomplete", "assemblies": uncached })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_entity::entry_point::Licenses;
    use gradient_entity::{EXAMPLE_ORIGINAL_HASH, EXAMPLE_STORED_HASH, RotatedHashMapping};
    use gradient_types::CacheId;
    use std::sync::Arc;

    const HELLO: &str = EXAMPLE_STORED_HASH;
    const GLIBC: &str = "1123456789abcdfghijklmnpqrsvwxyz";
    const ZLIB: &str = "2123456789abcdfghijklmnpqrsvwxyz";

    fn cached(hash: &str, package: &str, references: &[String]) -> MCachedPath {
        MCachedPath {
            hash: hash.into(),
            package: package.into(),
            nar_hash: Some(format!("sha256:{}", "0".repeat(52))),
            nar_size: Some(4096),
            references: Some(references.join(" ")),
            ..Default::default()
        }
    }

    fn named(name: &str, pname: &str) -> DerivationName {
        DerivationName {
            name: name.into(),
            pname: Some(pname.into()),
        }
    }

    fn hello_output() -> EntryPointOutput {
        EntryPointOutput {
            attr: "packages.x86_64-linux.hello".into(),
            licenses: Licenses(vec![License::Spdx {
                id: "GPL-3.0-or-later".into(),
            }]),
            hash: HELLO.into(),
            package: "hello-2.12.1".into(),
        }
    }

    fn input(reached: Vec<MCachedPath>) -> SbomInput {
        SbomInput {
            task: "project/task".into(),
            evaluation: EvaluationId::now_v7(),
            commit: Some("abc".into()),
            created_at: gradient_types::now(),
            entry_points: vec![hello_output()],
            reached: reached.into_iter().map(|p| (p.hash.clone(), p)).collect(),
            names: HashMap::from([
                (HELLO.to_owned(), named("hello-2.12.1", "hello")),
                (GLIBC.to_owned(), named("glibc-2.40", "glibc")),
            ]),
        }
    }

    fn hello_with_zlib() -> SbomInput {
        input(vec![
            cached(
                HELLO,
                "hello-2.12.1",
                &[
                    format!("{HELLO}-hello-2.12.1"),
                    format!("{GLIBC}-glibc-2.40"),
                    format!("{ZLIB}-zlib-1.3"),
                ],
            ),
            cached(GLIBC, "glibc-2.40", &[]),
        ])
    }

    fn component<'a>(doc: &'a Value, bom_ref: &str) -> &'a Value {
        doc["components"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["bom-ref"] == bom_ref)
            .unwrap_or_else(|| panic!("no component {bom_ref} in {doc}"))
    }

    fn depends_on(doc: &Value, bom_ref: &str) -> Value {
        doc["dependencies"]
            .as_array()
            .unwrap()
            .iter()
            .find(|d| d["ref"] == bom_ref)
            .unwrap()["dependsOn"]
            .clone()
    }

    #[test]
    fn an_entry_point_carries_its_version_purl_and_license() {
        let doc = cyclonedx(&hello_with_zlib());
        let hello = component(&doc, &format!("{HELLO}-hello-2.12.1"));

        assert_eq!(hello["name"], "hello");
        assert_eq!(hello["version"], "2.12.1");
        assert_eq!(hello["purl"], "pkg:nix/hello@2.12.1");
        assert_eq!(
            hello["licenses"],
            json!([{ "license": { "id": "GPL-3.0-or-later" } }])
        );
        assert_eq!(hello["hashes"][0]["content"], "0".repeat(64));
    }

    #[test]
    fn a_dependency_has_no_license() {
        let doc = cyclonedx(&hello_with_zlib());

        assert!(component(&doc, &format!("{GLIBC}-glibc-2.40")).get("licenses").is_none());
    }

    #[test]
    fn references_become_dependencies_without_the_path_itself() {
        let doc = cyclonedx(&hello_with_zlib());

        assert_eq!(
            depends_on(&doc, &format!("{HELLO}-hello-2.12.1")),
            json!([format!("{GLIBC}-glibc-2.40"), format!("{ZLIB}-zlib-1.3")])
        );
        assert_eq!(
            depends_on(&doc, TASK_REF),
            json!([format!("{HELLO}-hello-2.12.1")])
        );
    }

    #[test]
    fn an_uncached_reference_is_a_bare_component_that_marks_the_document_incomplete() {
        let doc = cyclonedx(&hello_with_zlib());
        let zlib_ref = format!("{ZLIB}-zlib-1.3");
        let zlib = component(&doc, &zlib_ref);

        assert!(zlib.get("hashes").is_none());
        assert_eq!(depends_on(&doc, &zlib_ref), json!([]));
        assert_eq!(doc["compositions"][0]["aggregate"], "incomplete");
        assert_eq!(doc["compositions"][0]["assemblies"], json!([zlib_ref]));
        assert_eq!(doc["metadata"]["properties"][0]["value"], "false");
    }

    #[test]
    fn a_fully_cached_closure_is_complete() {
        let doc = cyclonedx(&input(vec![cached(HELLO, "hello-2.12.1", &[])]));

        assert_eq!(doc["compositions"][0]["aggregate"], "complete");
        assert_eq!(doc["metadata"]["properties"][0]["value"], "true");
    }

    #[test]
    fn an_uncached_entry_point_output_is_still_listed() {
        let doc = cyclonedx(&input(vec![]));
        let hello = component(&doc, &format!("{HELLO}-hello-2.12.1"));

        assert_eq!(hello["licenses"][0]["license"]["id"], "GPL-3.0-or-later");
        assert_eq!(doc["compositions"][0]["aggregate"], "incomplete");
    }

    #[test]
    fn stored_hashes_are_replaced_by_the_original_ones() {
        let mapper = StorePathMapper::for_cache(Arc::new(RotatedHashMapping), CacheId::nil());
        let doc = cyclonedx(&with_original_hashes(hello_with_zlib(), &mapper));
        let text = doc.to_string();

        assert!(!text.contains(EXAMPLE_STORED_HASH), "{text}");
        assert!(text.contains(EXAMPLE_ORIGINAL_HASH), "{text}");
    }
}
