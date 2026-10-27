/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_wire::types::{DerivationOutput, DiscoveredDerivation};

pub fn discovered_derivation(
    attr: Option<String>,
    drv_path: String,
    drv: &crate::Derivation,
) -> DiscoveredDerivation {
    let outputs: Vec<DerivationOutput> = drv
        .outputs
        .iter()
        .map(|o| DerivationOutput {
            name: o.name.clone(),
            path: o.path.clone(),
        })
        .collect();

    let dependencies: Vec<String> = drv
        .input_derivations
        .iter()
        .map(|input| input.path.clone())
        .collect();

    let input_sources = drv.input_sources.clone();

    let meta = drv.build_meta();
    let name = drv.attr_string("name").unwrap_or_default();
    let pname = crate::derive_pname(drv.attr_string("pname").as_deref(), &name);
    DiscoveredDerivation {
        attr: attr.unwrap_or_default(),
        drv_path,
        outputs,
        dependencies,
        input_sources,
        architecture: drv.system.clone(),
        required_features: meta.required_features,
        timeout_secs: meta.timeout_secs,
        max_silent_secs: meta.max_silent_secs,
        prefer_local_build: meta.prefer_local_build,
        is_fixed_output: meta.is_fixed_output,
        allow_substitutes: drv.allow_substitutes(),
        pname,
        ifd: false,
        licenses: Vec::new(),
        runner: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovered_derivation_carries_input_sources() {
        let drv = crate::Derivation {
            outputs: vec![crate::DerivationOutput {
                name: "out".into(),
                path: "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-out".into(),
                hash_algo: String::new(),
                hash: String::new(),
            }],
            input_derivations: vec![],
            input_sources: vec![
                "/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-pipewire-extra-config".into(),
                "/nix/store/cccccccccccccccccccccccccccccccc-source-stdenv.sh".into(),
            ],
            system: "x86_64-linux".into(),
            builder: "/bin/sh".into(),
            args: vec![],
            environment: std::collections::HashMap::new(),
        };

        let discovered = discovered_derivation(
            Some("attr".into()),
            "/nix/store/dddddddddddddddddddddddddddddddd-foo.drv".into(),
            &drv,
        );

        assert_eq!(discovered.input_sources, drv.input_sources);
    }

    #[test]
    fn a_structured_attrs_derivation_reports_its_pname() {
        let drv = crate::Derivation {
            outputs: vec![],
            input_derivations: vec![],
            input_sources: vec![],
            system: "x86_64-linux".into(),
            builder: "/bin/sh".into(),
            args: vec![],
            environment: std::collections::HashMap::from([(
                "__json".to_owned(),
                r#"{"name":"hello-2.12.3","pname":"hello"}"#.to_owned(),
            )]),
        };

        let discovered =
            discovered_derivation(None, "/nix/store/[hash114]-hello-2.12.3.drv".into(), &drv);

        assert_eq!(discovered.pname.as_deref(), Some("hello"));
    }

    #[test]
    fn a_content_addressed_output_is_reported_without_a_path() {
        let drv = crate::Derivation {
            outputs: vec![crate::DerivationOutput {
                name: "out".into(),
                path: String::new(),
                hash_algo: "r:sha256".into(),
                hash: String::new(),
            }],
            input_derivations: vec![],
            input_sources: vec![],
            system: "x86_64-linux".into(),
            builder: "/bin/sh".into(),
            args: vec![],
            environment: std::collections::HashMap::new(),
        };

        let discovered = discovered_derivation(
            None,
            "/nix/store/dddddddddddddddddddddddddddddddd-ca.drv".into(),
            &drv,
        );

        assert_eq!(
            discovered.outputs,
            vec![DerivationOutput {
                name: "out".into(),
                path: String::new(),
            }]
        );
    }
}
