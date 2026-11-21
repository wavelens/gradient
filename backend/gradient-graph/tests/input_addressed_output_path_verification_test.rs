/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::HashMap;
use std::convert::Infallible;

use gradient_graph::derivation_closure_input_addressed_output_verification::{
    check_input_addressed_output_paths, hash_modulo_from_prefixed_storage_text,
    hash_modulo_to_prefixed_storage_text,
};
use harmonia_store_aterm::hash_modulo::{HashModulo, hash_input_modulo, hash_modulo};
use harmonia_store_aterm::input_address::make_output_path;
use harmonia_store_aterm::parse_derivation_aterm;
use harmonia_store_derivation::derivation::{Derivation, DerivationOutput};
use harmonia_store_path::{StoreDir, StorePath};

const DEP_ATERM: &[u8] = include_bytes!("../../gradient-ssh/testdata/input-addressed-dep.drv");
const DEP_PATH: &str = include_str!("../../gradient-ssh/testdata/input-addressed-dep.drv.path");
const TOP_ATERM: &[u8] = include_bytes!("../../gradient-ssh/testdata/input-addressed-top.drv");
const TOP_PATH: &str = include_str!("../../gradient-ssh/testdata/input-addressed-top.drv.path");
const GO_MODULES_ATERM: &[u8] = include_bytes!("../../gradient-ssh/testdata/go-modules.drv");
const GO_MODULES_PATH: &str = include_str!("../../gradient-ssh/testdata/go-modules.drv.path");

fn parse_derivation_with_store_path_into_pair(path: &str, text: &[u8]) -> (StorePath, Derivation) {
    let path = StorePath::from_base_path(path.trim().trim_start_matches("/nix/store/"))
        .expect("store path of an existing derivation file");
    let name = path.name().to_string();
    let derivation = parse_derivation_aterm(
        &StoreDir::default(),
        text,
        name.strip_suffix(".drv")
            .expect("file names of derivations end with .drv")
            .parse()
            .expect("valid derivation name"),
    )
    .expect("Nix wrote a valid derivation file");
    (path, derivation)
}

fn top_drv_text_with_one_byte_system_change() -> Vec<u8> {
    let text = std::str::from_utf8(TOP_ATERM).expect("Nix wrote a valid derivation file");
    let entry = "(\"system\",\"x86_64-linux\")";
    assert!(
        text.contains(entry),
        "The environment entries of top include (\"system\",\"x86_64-linux\")"
    );
    text.replace(entry, "(\"system\",\"x86_64-linuy\")")
        .into_bytes()
}

#[test]
fn test_real_input_addressed_dep_and_top_pass_recomputed_output_path_check() {
    let store_dir = StoreDir::default();
    let (dep_path, dep) = parse_derivation_with_store_path_into_pair(DEP_PATH, DEP_ATERM);
    let (top_path, top) = parse_derivation_with_store_path_into_pair(TOP_PATH, TOP_ATERM);
    let dep_hash = hash_input_modulo(&store_dir, &dep, |_| {
        Ok::<_, Infallible>(HashModulo::DeferredDrv)
    })
    .expect("hash modulo of the derivation");

    for derivation in [&dep, &top] {
        let drv_hash = hash_modulo(&store_dir, derivation, |input| {
            Ok::<_, Infallible>(if *input == dep_path {
                dep_hash.clone()
            } else {
                HashModulo::DeferredDrv
            })
        })
        .expect("hash modulo of the derivation")
        .expect("input-addressed derivation has hash modulo");
        for (output, stated) in &derivation.outputs {
            let DerivationOutput::InputAddressed(stated) = stated else {
                panic!("Each output of the real file is input-addressed");
            };
            assert_eq!(
                make_output_path(&store_dir, &derivation.name, &drv_hash, output)
                    .expect("Valid output path"),
                *stated,
            );
        }
    }

    check_input_addressed_output_paths(
        &HashMap::from([(dep_path, dep), (top_path, top)]),
        &HashMap::new(),
    )
    .expect("derivation files from Nix pass the output path check");
}

#[test]
fn closure_check_storage_text_matches_external_sha256sum_for_dep_and_top_derivations() {
    let (dep_path, dep) = parse_derivation_with_store_path_into_pair(DEP_PATH, DEP_ATERM);
    let (top_path, top) = parse_derivation_with_store_path_into_pair(TOP_PATH, TOP_ATERM);

    let hashes = check_input_addressed_output_paths(
        &HashMap::from([(dep_path.clone(), dep), (top_path.clone(), top)]),
        &HashMap::new(),
    )
    .expect("derivation files from Nix pass the output path check");

    assert_eq!(
        hash_modulo_to_prefixed_storage_text(&hashes[&dep_path]),
        "drv:sha256:2f147701429b5cb2d91db5743a8edbab066eedeb5f5a7afe7c5be32def86670c"
    );
    assert_eq!(
        hash_modulo_to_prefixed_storage_text(&hashes[&top_path]),
        "drv:sha256:93fde50a3163cc333d4ed43c059ac5b5a580b1dd33a6d88a6ab2d4988b54ddba"
    );
}

#[test]
fn parsing_stored_text_of_drv_fixed_and_deferred_hash_modulo_forms_round_trips_and_malformed_text_gives_none()
 {
    let (dep_path, dep) = parse_derivation_with_store_path_into_pair(DEP_PATH, DEP_ATERM);
    let (go_path, go_modules) =
        parse_derivation_with_store_path_into_pair(GO_MODULES_PATH, GO_MODULES_ATERM);
    let hashes = check_input_addressed_output_paths(
        &HashMap::from([(dep_path.clone(), dep), (go_path.clone(), go_modules)]),
        &HashMap::new(),
    )
    .expect("derivation files from Nix pass the output path check");

    for hash in [
        hashes[&dep_path].clone(),
        hashes[&go_path].clone(),
        HashModulo::DeferredDrv,
    ] {
        let text = hash_modulo_to_prefixed_storage_text(&hash);
        let parsed = hash_modulo_from_prefixed_storage_text(&text)
            .expect("Stored forms parse back to the same value.");
        assert_eq!(hash_modulo_to_prefixed_storage_text(&parsed), text);
    }
    for malformed in [
        "",
        "drv:sha256:00",
        "drv:md5:00",
        "fixed:",
        "fixed:out",
        "other",
    ] {
        assert!(
            hash_modulo_from_prefixed_storage_text(malformed).is_none(),
            "{malformed}"
        );
    }
}

#[test]
fn test_fixed_output_go_modules_drv_closure_check_returns_sha256_for_out() {
    let (path, go_modules) =
        parse_derivation_with_store_path_into_pair(GO_MODULES_PATH, GO_MODULES_ATERM);

    let hashes = check_input_addressed_output_paths(
        &HashMap::from([(path.clone(), go_modules)]),
        &HashMap::new(),
    )
    .expect("Files for a Nix fixed-output derivation pass the check on the output path.");

    assert_eq!(
        hash_modulo_to_prefixed_storage_text(&hashes[&path]),
        "fixed:out=sha256:c9ec3bb6818d9e1fe3bd8a0b10340674306912cb410093510f3d3f5148e3016e"
    );
}

#[test]
fn test_top_derivation_with_one_changed_environment_variable_byte_is_refused_by_output_path_check()
{
    let changed = top_drv_text_with_one_byte_system_change();
    check_input_addressed_output_paths(
        &HashMap::from([
            parse_derivation_with_store_path_into_pair(DEP_PATH, DEP_ATERM),
            parse_derivation_with_store_path_into_pair(TOP_PATH, &changed),
        ]),
        &HashMap::new(),
    )
    .expect_err("checks reject top with one environment byte changed");
}
