/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::mock::spec::{DaemonConfig, Node, Output};
use crate::mock::{artefact, store_path};
use harmonia_store_path::{StoreDir, StorePathSet};
use harmonia_store_path_info::{NarHash, fingerprint_path};
use harmonia_utils_hash::HashFormat as _;
use harmonia_utils_signature::SecretKey;
use std::path::Path;

const CACHE_INFO: &str = "StoreDir: /nix/store\nWantMassQuery: 1\nPriority: 30\n";

pub fn export(config: &DaemonConfig, secret_key: &str, out: &Path) -> anyhow::Result<usize> {
    let key: SecretKey = secret_key.trim().parse()?;
    std::fs::create_dir_all(out.join("nar"))?;
    std::fs::write(out.join("nix-cache-info"), CACHE_INFO)?;
    let mut count = 0;
    for (id, node) in config.derivations.iter().filter(|(_, n)| n.present.cache) {
        for name in node.outputs.keys() {
            export_output(&key, id, node, name, out)?;
            count += 1;
        }
    }
    Ok(count)
}

fn export_output(
    key: &SecretKey,
    id: &str,
    node: &Node,
    name: &str,
    out: &Path,
) -> anyhow::Result<()> {
    let output: &Output = &node.outputs[name];
    let nar = artefact::render(id, node, name)?;
    let path = store_path(&output.path)?;
    let hash = NarHash::digest(&nar);
    let refs = output
        .references
        .iter()
        .map(|r| store_path(r))
        .collect::<anyhow::Result<StorePathSet>>()?;
    let file = format!("nar/{}.nar", hash.as_base32().as_bare());
    std::fs::write(out.join(&file), &nar)?;

    let fingerprint = fingerprint_path(&StoreDir::default(), &path, &hash, nar.len() as u64, &refs);
    let refs_line = refs
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(" ");
    let narinfo = format!(
        "StorePath: {}\nURL: {file}\nCompression: none\nFileHash: {}\nFileSize: {}\nNarHash: {}\nNarSize: {}\nReferences: {refs_line}\nSig: {}\n",
        output.path,
        hash.as_base32(),
        nar.len(),
        hash.as_base32(),
        nar.len(),
        key.sign(&fingerprint),
    );
    std::fs::write(out.join(format!("{}.narinfo", path.hash())), narinfo)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> DaemonConfig {
        DaemonConfig::load(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/config.json"),
        )
        .expect("config")
    }

    #[test]
    fn exports_only_cache_nodes_with_valid_narinfo() {
        let mut config = fixture();
        config
            .derivations
            .get_mut("t/lib")
            .expect("lib")
            .present
            .cache = true;
        let dir = tempfile::tempdir().expect("tmp");
        let key = SecretKey::generate("test-1".into())
            .expect("key")
            .to_string();
        assert_eq!(export(&config, &key, dir.path()).expect("export"), 1);

        let lib = &config.derivations["t/lib"].outputs["out"].path;
        let hash = store_path(lib).expect("path").hash().to_string();
        let narinfo =
            std::fs::read_to_string(dir.path().join(format!("{hash}.narinfo"))).expect("narinfo");
        assert!(narinfo.contains(&format!("StorePath: {lib}")));
        assert!(narinfo.contains("NarHash: sha256:"));
        assert!(narinfo.contains("Sig: test-1:"));
        assert!(dir.path().join("nix-cache-info").exists());
    }
}
