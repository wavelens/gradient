/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::mock::nar::{NarFile, encode};
use crate::mock::spec::Node;
use crate::mock::timing::{key_hash, next};
use bytes::Bytes;
use std::collections::BTreeMap;

fn filler(seed: u64, key: &str, len: usize) -> Vec<u8> {
    let mut h = key_hash(seed, &[key]);
    let mut out = Vec::with_capacity(len + 8);
    while out.len() < len {
        h = next(h);
        out.extend_from_slice(&h.to_le_bytes());
    }
    out.truncate(len);
    out
}

fn plain(contents: impl Into<Vec<u8>>) -> NarFile {
    NarFile {
        contents: contents.into(),
        executable: false,
    }
}

pub fn render(node_id: &str, node: &Node, output: &str) -> anyhow::Result<Bytes> {
    let out = node
        .outputs
        .get(output)
        .ok_or_else(|| anyhow::anyhow!("{node_id} has no output {output}"))?;
    if node.fixed_output {
        let content = node
            .fod_content
            .clone()
            .ok_or_else(|| anyhow::anyhow!("{node_id}: FOD without fodContent"))?;
        return Ok(encode(&BTreeMap::from([(String::new(), plain(content))])));
    }

    let mut data =
        format!("gradient-daemon mock output\nnode: {node_id}\noutput: {output}\n").into_bytes();
    for reference in &out.references {
        data.extend_from_slice(format!("{reference}\n").as_bytes());
    }
    let pad = (out.size as usize).saturating_sub(data.len());
    data.extend(filler(node.timing.seed, &out.path, pad));

    let mut files = BTreeMap::from([("data".to_owned(), plain(data))]);
    if !out.products.is_empty() {
        let listing: String = out
            .products
            .iter()
            .map(|p| format!("{} {} {}/{}\n", p.kind, p.subtype, out.path, p.path))
            .collect();
        files.insert("nix-support/hydra-build-products".into(), plain(listing));
        for p in &out.products {
            let product = NarFile {
                contents: format!("product {}\n", p.path).into_bytes(),
                executable: true,
            };
            files.insert(p.path.clone(), product);
        }
    }

    Ok(encode(&files))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock::spec::DaemonConfig;
    use futures::TryStreamExt as _;
    use harmonia_file_nar::archive::{NarEvent, parse_nar};

    fn config() -> DaemonConfig {
        DaemonConfig::load(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/config.json"),
        )
        .expect("fixture")
    }

    #[test]
    fn rendering_is_deterministic() {
        let c = config();
        let node = &c.derivations["t/app"];
        assert_eq!(
            render("t/app", node, "out").expect("a"),
            render("t/app", node, "out").expect("b")
        );
    }

    #[test]
    fn output_carries_references_products_and_size() {
        let c = config();
        let app = &c.derivations["t/app"].outputs["out"];
        let lib = &c.derivations["t/lib"].outputs["out"];
        let nar = render("t/app", &c.derivations["t/app"], "out").expect("render");
        let text = String::from_utf8_lossy(&nar);
        assert!(text.contains(&lib.path));
        assert!(text.contains("hydra-build-products"));
        assert!(text.contains(&format!("file binary {}/bin/app", app.path)));
        assert!(nar.len() as u64 >= app.size);
    }

    async fn file_sizes(nar: Bytes) -> Vec<u64> {
        let mut events = std::pin::pin!(parse_nar(std::io::Cursor::new(nar)));
        let mut sizes = Vec::new();
        while let Some(event) = events.try_next().await.expect("parse") {
            if let NarEvent::File {
                size, mut reader, ..
            } = event
            {
                tokio::io::copy(&mut reader, &mut tokio::io::sink())
                    .await
                    .expect("file contents");
                sizes.push(size);
            }
        }
        sizes
    }

    #[tokio::test]
    async fn fixed_output_is_exactly_the_fod_content() {
        let mut node = config().derivations["t/lib"].clone();
        node.fixed_output = true;
        node.fod_content = Some("gradient-daemon fod t/lib\n".into());
        let nar = render("t/lib", &node, "out").expect("render");
        assert_eq!(file_sizes(nar).await, vec![26]);
    }

    #[tokio::test]
    async fn rendered_tree_parses_as_a_nar() {
        let c = config();
        let nar = render("t/app", &c.derivations["t/app"], "out").expect("render");
        assert_eq!(file_sizes(nar).await.len(), 3);
    }
}
