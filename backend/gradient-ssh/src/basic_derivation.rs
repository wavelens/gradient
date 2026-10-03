/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::session::Session;
use gradient_util::nar::single_file_nar;
use harmonia_protocol::valid_path_info::{UnkeyedValidPathInfo, ValidPathInfo};
use harmonia_store_aterm::print_derivation_aterm;
use harmonia_store_content_address::ContentAddress;
use harmonia_store_derivation::derivation::{BasicDerivation, DerivationInputs, DerivationT};
use harmonia_store_path::{StoreDir, StorePath};
use harmonia_store_path_info::NarHash;
use harmonia_utils_hash::Sha256;
use std::collections::BTreeMap;

struct DrvFile {
    path: StorePath,
    aterm: Vec<u8>,
    digest: Sha256,
}

fn drv_file(drv: &BasicDerivation) -> anyhow::Result<DrvFile> {
    let with_inputs = DerivationT {
        name: drv.name.clone(),
        outputs: drv.outputs.clone(),
        inputs: DerivationInputs::<StorePath> {
            srcs: drv.inputs.clone(),
            drvs: BTreeMap::new(),
        },
        platform: drv.platform.clone(),
        builder: drv.builder.clone(),
        args: drv.args.clone(),
        env: drv.env.clone(),
        structured_attrs: drv.structured_attrs.clone(),
    };
    let aterm = print_derivation_aterm(&StoreDir::default(), &with_inputs);
    let digest = Sha256::digest(&aterm);
    let path = gradient_daemon::ca_path::path_for(
        &format!("{}.drv", drv.name),
        &ContentAddress::Text(digest),
        &drv.inputs,
    )?;

    Ok(DrvFile {
        path,
        aterm,
        digest,
    })
}

pub async fn import(session: &Session, drv: &BasicDerivation) -> anyhow::Result<String> {
    let file = drv_file(drv)?;
    let nar = single_file_nar(&file.aterm, false);
    let store_dir = StoreDir::default();
    let info = ValidPathInfo {
        path: file.path.clone(),
        info: UnkeyedValidPathInfo {
            deriver: None,
            nar_hash: NarHash::digest(&nar),
            references: drv.inputs.clone(),
            registration_time: None,
            nar_size: nar.len() as u64,
            ultimate: false,
            signatures: Default::default(),
            ca: Some(ContentAddress::Text(file.digest)),
            store_dir: store_dir.clone(),
        },
    };

    crate::ingest::import(session, &info, nar.as_slice()).await?;
    Ok(store_dir.display(&file.path).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use harmonia_store_aterm::parse_derivation_aterm;
    use harmonia_store_derivation::derived_path::SingleDerivedPath;

    const ATERM: &[u8] = include_bytes!("../testdata/go-modules.drv");
    const PATH: &str = include_str!("../testdata/go-modules.drv.path");

    fn basic(aterm: &[u8]) -> BasicDerivation {
        let drv = parse_derivation_aterm(
            &StoreDir::default(),
            aterm,
            "go-modules".parse().expect("name"),
        )
        .expect("aterm");

        DerivationT {
            name: drv.name,
            outputs: drv.outputs,
            inputs: drv
                .inputs
                .into_iter()
                .filter_map(|input| match input {
                    SingleDerivedPath::Opaque(path) => Some(path),
                    SingleDerivedPath::Built { .. } => None,
                })
                .collect(),
            platform: drv.platform,
            builder: drv.builder,
            args: drv.args,
            env: drv.env,
            structured_attrs: drv.structured_attrs,
        }
    }

    #[test]
    fn a_fixed_output_derivation_lands_at_the_path_nix_gave_it() {
        let file = drv_file(&basic(ATERM)).expect("drv");

        assert_eq!(file.aterm, ATERM);
        assert_eq!(StoreDir::default().display(&file.path).to_string(), PATH);
    }
}
