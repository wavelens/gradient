/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! View type for [`BuildOutput`] NAR metadata that makes the pending/available
//! state explicit at the type level.
//!
//! [`BuildOutput::nar_size`] and [`BuildOutput::nar_hash`] are only both
//! `Some` after the worker has compressed the output and recorded the NAR.
//! [`BuildOutputMetadata`] groups these fields into a single enum so callers
//! can pattern-match instead of performing two independent `if let Some` checks
//! that could accidentally diverge.

use super::proto::BuildOutput;

/// Whether NAR metadata is available for a [`BuildOutput`].
///
/// Obtain via [`BuildOutput::nar_metadata`].
#[derive(Debug, Clone, PartialEq)]
pub enum BuildOutputMetadata<'a> {
    /// The NAR has not yet been processed — `nar_size` and `nar_hash` are both
    /// absent. This is the normal state immediately after a build completes,
    /// before the worker compresses and hashes the output NAR.
    Pending,

    /// Both `nar_size` and `nar_hash` are present — the NAR has been
    /// compressed, hashed, and (typically) uploaded to the cache.
    Available {
        /// Uncompressed NAR size in bytes.
        nar_size: i64,
        /// NAR hash in `sha256:<nix32>` format.
        nar_hash: &'a str,
    },
}

impl BuildOutput {
    /// Return a view of this output's NAR metadata.
    ///
    /// Returns [`BuildOutputMetadata::Available`] only when both `nar_size`
    /// and `nar_hash` are present; returns [`BuildOutputMetadata::Pending`]
    /// otherwise.
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

// ── Tests ─────────────────────────────────────────────────────────────────────

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
            has_artefacts: false,
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
