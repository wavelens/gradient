/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::types::BuildOutput;

#[derive(Debug, Clone, PartialEq)]
pub enum BuildOutputMetadata<'a> {
    Pending,

    Available { nar_size: i64, nar_hash: &'a str },
}

impl BuildOutput {
    pub fn nar_metadata(&self) -> BuildOutputMetadata<'_> {
        match (&self.nar_hash, self.nar_size) {
            (Some(hash), Some(size)) => BuildOutputMetadata::Available {
                nar_size: size,
                nar_hash: hash.as_str(),
            },
            _ => BuildOutputMetadata::Pending,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_output() -> BuildOutput {
        BuildOutput {
            name: "out".into(),
            store_path: "/nix/store/aaaa-pkg".into(),
            hash: "aaaa".into(),
            nar_size: None,
            nar_hash: None,
            products: vec![],
        }
    }

    #[test]
    fn nar_metadata_pending_when_both_absent() {
        let o = base_output();
        assert_eq!(o.nar_metadata(), BuildOutputMetadata::Pending);
    }

    #[test]
    fn nar_metadata_pending_when_only_size() {
        let o = BuildOutput {
            nar_size: Some(42),
            ..base_output()
        };
        assert_eq!(o.nar_metadata(), BuildOutputMetadata::Pending);
    }

    #[test]
    fn nar_metadata_pending_when_only_hash() {
        let o = BuildOutput {
            nar_hash: Some("sha256:0mdqa9w1p6cmli6976v4wi0sw9r4p5prkj7lzfd1877wk11c9c73".into()),
            ..base_output()
        };
        assert_eq!(o.nar_metadata(), BuildOutputMetadata::Pending);
    }

    #[test]
    fn nar_metadata_available_when_both_present() {
        let o = BuildOutput {
            nar_size: Some(1024),
            nar_hash: Some("sha256:0mdqa9w1p6cmli6976v4wi0sw9r4p5prkj7lzfd1877wk11c9c73".into()),
            ..base_output()
        };
        match o.nar_metadata() {
            BuildOutputMetadata::Available { nar_size, nar_hash } => {
                assert_eq!(nar_size, 1024);
                assert_eq!(
                    nar_hash,
                    "sha256:0mdqa9w1p6cmli6976v4wi0sw9r4p5prkj7lzfd1877wk11c9c73"
                );
            }
            BuildOutputMetadata::Pending => panic!("expected Available"),
        }
    }
}
