/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::{BTreeMap, HashMap};
use std::convert::Infallible;
use std::sync::{LazyLock, Mutex};

use gradient_util::nar::single_file_nar;
use gradient_wire::content_addressed_store_path_from_name_address_references::path_for;
use gradient_wire::types::{DerivationOutput, DiscoveredDerivation};
use harmonia_store_aterm::hash_modulo::{HashModulo, hash_input_modulo, hash_modulo};
use harmonia_store_aterm::input_address::make_output_path;
use harmonia_store_aterm::parse_derivation_aterm;
use harmonia_store_content_address::ContentAddress;
use harmonia_store_derivation::derivation::Derivation;
use harmonia_store_path::{StoreDir, StorePath, StorePathSet};
use harmonia_utils_hash::Sha256;

const STORE_DIR: &str = "/nix/store/";

static GENERATED_DERIVATION_HASH_MODULO_BY_STORE_PATH: LazyLock<
    Mutex<HashMap<String, HashModulo>>,
> = LazyLock::new(Default::default);

#[derive(Debug, Clone, PartialEq)]
pub struct DerivationFileFromReportedFacts {
    pub derivation: DiscoveredDerivation,
    pub text: Vec<u8>,
}

impl DerivationFileFromReportedFacts {
    pub fn from_reported_derivation_facts(reported: &DiscoveredDerivation) -> Self {
        let base = reported
            .drv_path
            .strip_prefix(STORE_DIR)
            .unwrap_or(&reported.drv_path);
        let (_, name) = base
            .split_once('-')
            .unwrap_or_else(|| panic!("{} is no store path", reported.drv_path));
        let derivation_name_without_drv_suffix = name.trim_end_matches(".drv");
        let output_paths = compute_output_store_paths_from_content_and_input_derivations(
            reported,
            derivation_name_without_drv_suffix,
        )
        .unwrap_or_else(|| {
            reported
                .outputs
                .iter()
                .map(|output| (output.name.clone(), output.path.clone()))
                .collect()
        });
        let text = aterm(reported, derivation_name_without_drv_suffix, &output_paths);
        let path =
            text_addressed_drv_store_path_from_name_text_and_references(name, &text, reported);
        let drv = parse_generated_derivation_or_panic(&text, derivation_name_without_drv_suffix);
        let input_hash = {
            let known = GENERATED_DERIVATION_HASH_MODULO_BY_STORE_PATH
                .lock()
                .expect(
                    "No thread panicked while holding the lock on the generated derivation map.",
                );
            hash_input_modulo(&StoreDir::default(), &drv, |input| {
                Ok::<_, Infallible>(
                    recorded_hash_modulo_for_input_derivation_store_path_or_deferred(&known, input),
                )
            })
            .expect("Hash Modulo of the Generated Derivation")
        };
        GENERATED_DERIVATION_HASH_MODULO_BY_STORE_PATH
            .lock()
            .expect("No thread panicked while holding the lock on the generated derivation map.")
            .insert(path.to_string(), input_hash);
        Self {
            derivation: DiscoveredDerivation {
                drv_path: format!("{STORE_DIR}{path}"),
                dependencies: reported.dependencies.iter().map(|d| full(d)).collect(),
                outputs: reported
                    .outputs
                    .iter()
                    .map(|output| DerivationOutput {
                        name: output.name.clone(),
                        path: output_paths[&output.name].clone(),
                    })
                    .collect(),
                ..reported.clone()
            },
            text,
        }
    }

    pub fn from_drv_text_and_reported_facts(
        text: Vec<u8>,
        reported: &DiscoveredDerivation,
    ) -> Self {
        let base = reported
            .drv_path
            .strip_prefix(STORE_DIR)
            .unwrap_or(&reported.drv_path);
        let (_, name) = base
            .split_once('-')
            .unwrap_or_else(|| panic!("{} is no store path", reported.drv_path));
        let path =
            text_addressed_drv_store_path_from_name_text_and_references(name, &text, reported);
        Self {
            derivation: DiscoveredDerivation {
                drv_path: format!("{STORE_DIR}{path}"),
                ..reported.clone()
            },
            text,
        }
    }

    pub fn from_nix_written_derivation_file_bytes_and_store_path(text: &[u8], path: &str) -> Self {
        let drv = gradient_derivation::parse_drv(text)
            .unwrap_or_else(|e| panic!("{path} is not a derivation file: {e}"));
        Self {
            derivation: gradient_derivation::discovered_derivation(
                None,
                path.trim().to_owned(),
                &drv,
            ),
            text: text.to_vec(),
        }
    }

    pub fn uncompressed_nar_archive_bytes(&self) -> Vec<u8> {
        single_file_nar(&self.text, false)
    }
}

fn text_addressed_drv_store_path_from_name_text_and_references(
    name: &str,
    text: &[u8],
    reported: &DiscoveredDerivation,
) -> StorePath {
    let references: StorePathSet = reported
        .dependencies
        .iter()
        .chain(&reported.input_sources)
        .map(|path| full(path))
        .map(|path| {
            StorePath::from_base_path(&path[STORE_DIR.len()..])
                .unwrap_or_else(|e| panic!("{path}: {e}"))
        })
        .collect();
    path_for(
        name,
        &ContentAddress::Text(Sha256::digest(text)),
        &references,
    )
    .expect("a text store path")
}

fn compute_output_store_paths_from_content_and_input_derivations(
    reported: &DiscoveredDerivation,
    name: &str,
) -> Option<BTreeMap<String, String>> {
    let store_dir = StoreDir::default();
    let unfilled = reported
        .outputs
        .iter()
        .map(|output| (output.name.clone(), String::new()))
        .collect();
    let drv = parse_generated_derivation_or_panic(&aterm(reported, name, &unfilled), name);
    let drv_hash = if reported.is_fixed_output {
        None
    } else {
        let known = GENERATED_DERIVATION_HASH_MODULO_BY_STORE_PATH
            .lock()
            .expect("No thread panicked while holding the lock on the generated derivation map.");
        Some(
            hash_modulo(&store_dir, &drv, |input| {
                Ok::<_, Infallible>(
                    recorded_hash_modulo_for_input_derivation_store_path_or_deferred(&known, input),
                )
            })
            .expect("Hash Modulo of the Generated Derivation")?,
        )
    };
    drv.outputs
        .iter()
        .map(|(output, kind)| {
            let path = match &drv_hash {
                Some(drv_hash) => make_output_path(&store_dir, &drv.name, drv_hash, output),
                None => kind
                    .path(&store_dir, &drv.name, output)
                    .map(|path| path.expect("A fixed output has a path.")),
            }
            .expect("Validity of the Name for the Output Path");
            Some((output.to_string(), format!("{STORE_DIR}{path}")))
        })
        .collect()
}

fn recorded_hash_modulo_for_input_derivation_store_path_or_deferred(
    known: &HashMap<String, HashModulo>,
    input: &StorePath,
) -> HashModulo {
    known
        .get(&input.to_string())
        .cloned()
        .unwrap_or(HashModulo::DeferredDrv)
}

fn parse_generated_derivation_or_panic(text: &[u8], name: &str) -> Derivation {
    parse_derivation_aterm(
        &StoreDir::default(),
        text,
        name.parse().expect("a valid derivation name"),
    )
    .unwrap_or_else(|e| panic!("The generated derivation {name} is valid: {e}"))
}

fn full(path: &str) -> String {
    if path.starts_with(STORE_DIR) {
        path.to_owned()
    } else {
        format!("{STORE_DIR}{path}")
    }
}

fn quoted(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

fn list(items: impl IntoIterator<Item = String>) -> String {
    format!("[{}]", items.into_iter().collect::<Vec<_>>().join(","))
}

fn aterm(
    reported: &DiscoveredDerivation,
    name: &str,
    output_paths: &BTreeMap<String, String>,
) -> Vec<u8> {
    let mut outputs: Vec<_> = reported.outputs.iter().collect();
    outputs.sort_by(|a, b| a.name.cmp(&b.name));
    let (hash_algo, hash) = if reported.is_fixed_output {
        ("sha256", "0".repeat(52))
    } else {
        ("", String::new())
    };
    let outputs = list(outputs.into_iter().map(|output| {
        format!(
            "({},{},{},{})",
            quoted(&output.name),
            quoted(&output_paths[&output.name]),
            quoted(hash_algo),
            quoted(&hash)
        )
    }));

    let mut inputs: Vec<(String, &[String])> = reported
        .dependencies
        .iter()
        .enumerate()
        .map(|(at, path)| {
            let wanted = reported
                .dependency_outputs
                .get(at)
                .map_or(&[][..], Vec::as_slice);
            (full(path), wanted)
        })
        .collect();
    inputs.sort();
    let inputs = list(inputs.into_iter().map(|(path, wanted)| {
        format!(
            "({},{})",
            quoted(&path),
            list(wanted.iter().map(|o| quoted(o)))
        )
    }));

    let mut sources: Vec<String> = reported.input_sources.iter().map(|s| full(s)).collect();
    sources.sort();

    let mut environment = BTreeMap::from([("name", name.to_owned())]);
    for (output, path) in output_paths {
        environment.insert(output.as_str(), path.clone());
    }
    if let Some(pname) = &reported.pname {
        environment.insert("pname", pname.clone());
    }
    if !reported.required_features.is_empty() {
        environment.insert(
            "requiredSystemFeatures",
            reported.required_features.join(" "),
        );
    }
    if let Some(timeout) = reported.timeout_secs {
        environment.insert("timeout", timeout.to_string());
    }
    if let Some(max_silent) = reported.max_silent_secs {
        environment.insert("maxSilent", max_silent.to_string());
    }
    if reported.prefer_local_build {
        environment.insert("preferLocalBuild", "1".to_owned());
    }
    if !reported.allow_substitutes {
        environment.insert("allowSubstitutes", String::new());
    }
    let environment = list(
        environment
            .into_iter()
            .map(|(key, value)| format!("({},{})", quoted(key), quoted(&value))),
    );

    format!(
        "Derive({outputs},{inputs},{sources},{},{},[],{environment})",
        quoted(&reported.architecture),
        quoted("/bin/sh"),
        sources = list(sources.iter().map(|s| quoted(s))),
    )
    .into_bytes()
}
